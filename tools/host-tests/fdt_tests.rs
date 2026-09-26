//! Tests for src/fdt.rs.

use crate::fdt::{Fdt, FdtError, Node, cell_pairs};

const FIXTURE: &[u8] = include_bytes!("fixtures/pi-like.dtb");

fn fixture() -> Fdt<'static> {
    Fdt::new(FIXTURE).expect("fixture parses")
}

fn string(value: &[u8]) -> &str {
    std::str::from_utf8(value).unwrap().trim_end_matches('\0')
}

#[test]
fn header_and_size() {
    assert_eq!(Fdt::total_size(FIXTURE), Ok(FIXTURE.len()));
    assert_eq!(fixture().size(), FIXTURE.len());
    assert_eq!(Fdt::total_size(&[0; 8]), Err(FdtError::BadMagic));
    assert_eq!(Fdt::total_size(&[0xd0, 0x0d]), Err(FdtError::Truncated));
}

#[test]
fn memory_reservation_block() {
    let r: Vec<_> = fixture().reservations().collect();
    assert_eq!(r, [(0, 0x1000), (0x2eff_0000, 0x1_0000)]);
}

#[test]
fn root_properties() {
    let root = fixture().root();
    assert_eq!(root.name(), "");
    assert_eq!(string(root.property("model").unwrap()), "Raspberry Pi 4 Model B");
    assert_eq!(root.cells(), (2, 1));
    let names: Vec<_> = root.children().map(|c| c.name()).collect();
    assert_eq!(names, ["memory@0", "reserved-memory", "chosen", "soc"]);
}

#[test]
fn memory_node_by_base_name() {
    let fdt = fixture();
    let memory = fdt.find("/memory").unwrap();
    assert_eq!(memory.name(), "memory@0");
    assert_eq!(fdt.find("/memory@0").unwrap().name(), "memory@0");
    assert!(fdt.find("/memory@1").is_none());
    let (a, s) = fdt.root().cells();
    let reg: Vec<_> = memory.reg(a, s).collect();
    assert_eq!(reg, [(0, 0x3b40_0000), (0x4000_0000, 0xbc00_0000), (0x1_0000_0000, 0x8000_0000)]);
}

#[test]
fn reserved_memory_children() {
    let rm = fixture().find("/reserved-memory").unwrap();
    let (a, s) = rm.cells();
    let children: Vec<Node> = rm.children().collect();
    assert_eq!(children.len(), 2);
    assert_eq!(children[0].name(), "atf@0");
    assert_eq!(children[0].reg(a, s).collect::<Vec<_>>(), [(0, 0x80000)]);
    assert!(children[0].property("no-map").is_some());
    assert_eq!(children[1].name(), "linux,cma");
    assert_eq!(children[1].reg(a, s).count(), 0, "a dynamic reservation has no reg");
}

#[test]
fn chosen_and_nested_paths() {
    let fdt = fixture();
    let chosen = fdt.find("/chosen").unwrap();
    assert_eq!(chosen.u32_property("linux,initrd-start"), Some(0x0200_0000));
    assert_eq!(chosen.u32_property("linux,initrd-end"), Some(0x0210_0000));
    assert_eq!(chosen.u32_property("bootargs"), None, "not a single cell");
    let soc = fdt.find("/soc").unwrap();
    let serial = fdt.find("/soc/serial@7e201000").unwrap();
    let (a, s) = soc.cells();
    assert_eq!(serial.reg(a, s).collect::<Vec<_>>(), [(0x7e20_1000, 0x200)]);
    assert!(fdt.find("/soc/serial").is_some());
    assert!(fdt.find("/nonexistent").is_none());
    assert!(fdt.find("/chosen/deeper").is_none());
}

#[test]
fn cell_pairs_widths() {
    let bytes = [0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3];
    assert_eq!(cell_pairs(&bytes, 2, 1).collect::<Vec<_>>(), [(0x1_0000_0002, 3)]);
    assert_eq!(cell_pairs(&bytes, 1, 1).collect::<Vec<_>>(), [(1, 2)], "trailing partial pair ignored");
    assert_eq!(cell_pairs(&bytes, 1, 0).collect::<Vec<_>>(), [(1, 0), (2, 0), (3, 0)]);
    assert_eq!(cell_pairs(&bytes, 3, 1).count(), 0, "unsupported widths yield nothing");
    assert_eq!(cell_pairs(&bytes, 0, 1).count(), 0);
}

/// Visit every node and property; returns how many nodes there were.
fn walk(node: Node, depth: usize) -> usize {
    assert!(depth < 64, "runaway recursion");
    for p in node.properties() {
        let _ = (p.name.len(), p.value.len());
    }
    let _ = node.cells();
    1 + node.children().map(|c| walk(c, depth + 1)).sum::<usize>()
}

#[test]
fn every_truncation_is_rejected_or_harmless() {
    for len in 0..FIXTURE.len() {
        assert!(Fdt::new(&FIXTURE[..len]).is_err(), "length {len} should be too short");
        // Also claim the shorter length in the header, so the parser has
        // to cope with the structure ending early.
        let mut bytes = FIXTURE[..len].to_vec();
        if len >= 8 {
            bytes[4..8].copy_from_slice(&(len as u32).to_be_bytes());
        }
        if let Ok(fdt) = Fdt::new(&bytes) {
            walk(fdt.root(), 0);
            let _ = fdt.reservations().count();
        }
    }
}

#[test]
fn every_single_byte_corruption_is_rejected_or_harmless() {
    for pos in 0..FIXTURE.len() {
        for flip in [0x01, 0x80, 0xff] {
            let mut bytes = FIXTURE.to_vec();
            bytes[pos] ^= flip;
            if let Ok(fdt) = Fdt::new(&bytes) {
                walk(fdt.root(), 0);
                let _ = fdt.reservations().take(1000).count();
                let _ = fdt.find("/reserved-memory/atf@0");
            }
        }
    }
}

/// The real device trees, if `make sdcard` has downloaded them.
#[test]
fn raspberry_pi_device_trees() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../build/sdcard");
    let mut checked = 0;
    for name in ["bcm2711-rpi-4-b.dtb", "bcm2712-rpi-5-b.dtb", "bcm2712d0-rpi-5-b.dtb", "bcm2712-rpi-500.dtb"] {
        let Ok(bytes) = std::fs::read(format!("{dir}/{name}")) else { continue };
        let fdt = Fdt::new(&bytes).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert!(walk(fdt.root(), 0) > 50, "{name}");
        assert!(fdt.find("/memory").is_some(), "{name}: /memory");
        assert!(fdt.find("/chosen").is_some(), "{name}: /chosen");
        let rm = fdt.find("/reserved-memory").unwrap();
        assert!(rm.children().count() >= 1, "{name}: reserved-memory children");
        checked += 1;
    }
    if name_is_pi5_present(dir) {
        let bytes = std::fs::read(format!("{dir}/bcm2712-rpi-5-b.dtb")).unwrap();
        let fdt = Fdt::new(&bytes).unwrap();
        let rm = fdt.find("/reserved-memory").unwrap();
        let (a, s) = rm.cells();
        let atf = rm.child("atf").expect("Pi 5 reserves memory for TF-A");
        assert_eq!(atf.reg(a, s).collect::<Vec<_>>(), [(0, 0x80000)]);
        assert!(atf.property("no-map").is_some());
    }
    eprintln!("checked {checked} Raspberry Pi device trees");
}

fn name_is_pi5_present(dir: &str) -> bool {
    std::path::Path::new(&format!("{dir}/bcm2712-rpi-5-b.dtb")).exists()
}
