//! A parser for the flattened device tree (DTB) the firmware passes us.
//!
//! Format: the Devicetree Specification, chapter 5
//! (https://devicetree-specification.readthedocs.io). All values are
//! big-endian.
//!
//! Written in safe Rust over a byte slice: every read is bounds-checked, so
//! a malformed device tree produces an error or missing data, never a
//! crash or an out-of-bounds read. `Fdt::new` checks the whole structure
//! once up front. This file depends only on `core`, so the host unit tests
//! compile it too.

const MAGIC: u32 = 0xD00D_FEED;
const HEADER_SIZE: usize = 40;
/// The oldest format version whose layout we understand.
const MIN_VERSION: u32 = 16;

const FDT_BEGIN_NODE: u32 = 1;
const FDT_END_NODE: u32 = 2;
const FDT_PROP: u32 = 3;
const FDT_NOP: u32 = 4;
const FDT_END: u32 = 9;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FdtError {
    BadMagic,
    UnsupportedVersion,
    Truncated,
    Malformed,
}

/// A parsed device tree, borrowing its bytes.
#[derive(Clone, Copy)]
pub struct Fdt<'a> {
    data: &'a [u8],
    structure: &'a [u8],
    strings: &'a [u8],
    reservations: &'a [u8],
}

fn be32(bytes: &[u8], offset: usize) -> Option<u32> {
    let b = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be64(bytes: &[u8], offset: usize) -> Option<u64> {
    Some(((be32(bytes, offset)? as u64) << 32) | be32(bytes, offset.checked_add(4)?)? as u64)
}

/// The NUL-terminated string starting at `offset`.
fn c_str(bytes: &[u8], offset: usize) -> Option<&str> {
    let rest = bytes.get(offset..)?;
    let len = rest.iter().position(|&b| b == 0)?;
    core::str::from_utf8(&rest[..len]).ok()
}

impl<'a> Fdt<'a> {
    /// The total size recorded in a device tree header, to know how many
    /// bytes to hand to `new`. Needs the first 8 bytes.
    pub fn total_size(header: &[u8]) -> Result<usize, FdtError> {
        match be32(header, 0) {
            Some(MAGIC) => be32(header, 4)
                .map(|n| n as usize)
                .ok_or(FdtError::Truncated),
            Some(_) => Err(FdtError::BadMagic),
            None => Err(FdtError::Truncated),
        }
    }

    pub fn new(data: &'a [u8]) -> Result<Self, FdtError> {
        let size = Self::total_size(data)?;
        if size < HEADER_SIZE || size > data.len() {
            return Err(FdtError::Truncated);
        }
        let data = &data[..size];
        let field = |n: usize| {
            be32(data, n * 4)
                .map(|v| v as usize)
                .ok_or(FdtError::Truncated)
        };
        let (off_struct, off_strings, off_reserve) = (field(2)?, field(3)?, field(4)?);
        let (version, last_compatible) = (field(5)? as u32, field(6)? as u32);
        let (size_strings, size_struct) = (field(8)?, field(9)?);
        if version < MIN_VERSION && last_compatible < MIN_VERSION {
            return Err(FdtError::UnsupportedVersion);
        }
        let slice = |start: usize, len: usize| {
            data.get(start..start.checked_add(len).ok_or(FdtError::Malformed)?)
                .ok_or(FdtError::Truncated)
        };
        let fdt = Fdt {
            data,
            structure: slice(off_struct, size_struct)?,
            strings: slice(off_strings, size_strings)?,
            reservations: data.get(off_reserve..).ok_or(FdtError::Truncated)?,
        };
        fdt.validate()?;
        Ok(fdt)
    }

    /// Walk every token once: nodes must nest properly, names and property
    /// names must be valid, and the block must end with FDT_END.
    fn validate(&self) -> Result<(), FdtError> {
        let mut offset = 0;
        let mut depth = 0usize;
        let mut seen_root = false;
        loop {
            let (token, next) = self.token(offset).ok_or(FdtError::Malformed)?;
            match token {
                Token::BeginNode(_) => {
                    if depth == 0 && seen_root {
                        return Err(FdtError::Malformed); // only one root
                    }
                    seen_root = true;
                    depth += 1;
                }
                Token::EndNode => depth = depth.checked_sub(1).ok_or(FdtError::Malformed)?,
                Token::Property(_) if depth == 0 => return Err(FdtError::Malformed),
                Token::Property(_) | Token::Nop => {}
                Token::End => {
                    return if depth == 0 && seen_root {
                        Ok(())
                    } else {
                        Err(FdtError::Malformed)
                    };
                }
            }
            offset = next;
        }
    }

    /// Decode the token at `offset` in the structure block, returning it and
    /// the offset of the next one.
    fn token(&self, offset: usize) -> Option<(Token<'a>, usize)> {
        let s = self.structure;
        let align = |n: usize| n.checked_next_multiple_of(4);
        match be32(s, offset)? {
            FDT_BEGIN_NODE => {
                let name = c_str(s, offset + 4)?;
                Some((Token::BeginNode(name), align(offset + 4 + name.len() + 1)?))
            }
            FDT_END_NODE => Some((Token::EndNode, offset + 4)),
            FDT_PROP => {
                let len = be32(s, offset + 4)? as usize;
                let name = c_str(self.strings, be32(s, offset + 8)? as usize)?;
                let start = offset + 12;
                let value = s.get(start..start.checked_add(len)?)?;
                Some((
                    Token::Property(Property { name, value }),
                    align(start + len)?,
                ))
            }
            FDT_NOP => Some((Token::Nop, offset + 4)),
            FDT_END => Some((Token::End, offset + 4)),
            _ => None,
        }
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    /// The memory reservation block: (address, size) ranges not to use.
    pub fn reservations(&self) -> impl Iterator<Item = (u64, u64)> + 'a {
        let block = self.reservations;
        let mut offset = 0;
        core::iter::from_fn(move || {
            let (addr, size) = (be64(block, offset)?, be64(block, offset + 8)?);
            if addr == 0 && size == 0 {
                return None;
            }
            offset += 16;
            Some((addr, size))
        })
    }

    pub fn root(&self) -> Node<'a> {
        // `validate` guarantees the structure starts (after any NOPs) with
        // the root node.
        let mut offset = 0;
        loop {
            match self.token(offset) {
                Some((Token::BeginNode(name), body)) => {
                    return Node {
                        fdt: *self,
                        name,
                        body,
                    };
                }
                Some((_, next)) => offset = next,
                None => {
                    return Node {
                        fdt: *self,
                        name: "",
                        body: self.structure.len(),
                    };
                }
            }
        }
    }

    /// The node at an absolute path such as `/reserved-memory`. A path
    /// component without a unit address (`memory`) also matches one with it
    /// (`memory@0`).
    pub fn find(&self, path: &str) -> Option<Node<'a>> {
        path.split('/')
            .filter(|c| !c.is_empty())
            .try_fold(self.root(), |node, name| node.child(name))
    }
}

#[derive(Clone, Copy)]
enum Token<'a> {
    BeginNode(&'a str),
    EndNode,
    Property(Property<'a>),
    Nop,
    End,
}

#[derive(Clone, Copy, Debug)]
pub struct Property<'a> {
    pub name: &'a str,
    pub value: &'a [u8],
}

/// A node in the tree.
#[derive(Clone, Copy)]
pub struct Node<'a> {
    fdt: Fdt<'a>,
    name: &'a str,
    /// Offset of the node's first property or child.
    body: usize,
}

impl<'a> Node<'a> {
    /// The full name, including any unit address (`memory@0`).
    pub fn name(&self) -> &'a str {
        self.name
    }

    /// The name without the unit address (`memory`).
    pub fn base_name(&self) -> &'a str {
        self.name.split('@').next().unwrap_or(self.name)
    }

    pub fn properties(&self) -> impl Iterator<Item = Property<'a>> + 'a {
        let fdt = self.fdt;
        let mut offset = self.body;
        core::iter::from_fn(move || {
            loop {
                match fdt.token(offset)? {
                    (Token::Property(p), next) => {
                        offset = next;
                        return Some(p);
                    }
                    (Token::Nop, next) => offset = next,
                    _ => return None,
                }
            }
        })
    }

    pub fn property(&self, name: &str) -> Option<&'a [u8]> {
        self.properties().find(|p| p.name == name).map(|p| p.value)
    }

    /// A property holding a single 32-bit cell.
    pub fn u32_property(&self, name: &str) -> Option<u32> {
        match self.property(name)? {
            value if value.len() == 4 => be32(value, 0),
            _ => None,
        }
    }

    pub fn children(&self) -> impl Iterator<Item = Node<'a>> + 'a {
        let fdt = self.fdt;
        let mut offset = self.body;
        core::iter::from_fn(move || {
            loop {
                match fdt.token(offset)? {
                    (Token::BeginNode(name), body) => {
                        offset = fdt.skip_node(body)?;
                        return Some(Node { fdt, name, body });
                    }
                    (Token::Property(_) | Token::Nop, next) => offset = next,
                    _ => return None,
                }
            }
        })
    }

    /// The child called `name` (see `Fdt::find` on unit addresses).
    pub fn child(&self, name: &str) -> Option<Node<'a>> {
        let exact = name.contains('@');
        self.children().find(|c| {
            if exact {
                c.name == name
            } else {
                c.name == name || c.base_name() == name
            }
        })
    }

    /// The node's `reg` property as (address, size) pairs, given the
    /// `#address-cells` and `#size-cells` of its parent.
    pub fn reg(
        &self,
        address_cells: u32,
        size_cells: u32,
    ) -> impl Iterator<Item = (u64, u64)> + 'a {
        let value = self.property("reg").unwrap_or(&[]);
        cell_pairs(value, address_cells, size_cells)
    }

    /// This node's `#address-cells` and `#size-cells`, which apply to its
    /// children's `reg` properties (defaults 2 and 1).
    pub fn cells(&self) -> (u32, u32) {
        (
            self.u32_property("#address-cells").unwrap_or(2),
            self.u32_property("#size-cells").unwrap_or(1),
        )
    }
}

impl<'a> Fdt<'a> {
    /// Given the offset just after a node's name, return the offset just
    /// after its END_NODE.
    fn skip_node(&self, mut offset: usize) -> Option<usize> {
        let mut depth = 1usize;
        while depth > 0 {
            let (token, next) = self.token(offset)?;
            match token {
                Token::BeginNode(_) => depth += 1,
                Token::EndNode => depth -= 1,
                Token::End => return None,
                _ => {}
            }
            offset = next;
        }
        Some(offset)
    }
}

/// Decode `value` as a list of (address, size) pairs of the given widths in
/// 32-bit cells (each at most 2). Stops at anything that doesn't fit.
pub fn cell_pairs(
    value: &[u8],
    address_cells: u32,
    size_cells: u32,
) -> impl Iterator<Item = (u64, u64)> + '_ {
    let (a, s) = (address_cells as usize, size_cells as usize);
    let stride = (a + s) * 4;
    let valid = (1..=2).contains(&a) && s <= 2;
    let read = |bytes: &[u8], cells: usize| -> Option<u64> {
        match cells {
            0 => Some(0),
            1 => be32(bytes, 0).map(u64::from),
            _ => be64(bytes, 0),
        }
    };
    let chunks = if valid {
        value.chunks_exact(stride)
    } else {
        [].chunks_exact(1)
    };
    chunks.map_while(move |chunk| Some((read(chunk, a)?, read(&chunk[a * 4..], s)?)))
}
