//! How `init` sets up a USB driver, through a handle with
//! [`SETUP_BADGE`](crate::console::SETUP_BADGE) to the endpoint the driver
//! was started with. Each is a call, answered `OK` or `REFUSED`:
//!
//! | Label | Handle | Data |
//! | --- | --- | --- |
//! | `SETUP_CONTROLLER` | the controller's registers | kind (`pios_abi::USB_XHCI` or `USB_DWC3`), size, controller number |
//! | `SETUP_DMA` | DMA handle for the controller | |
//! | `SETUP_INPUT` | console handle with `INPUT_BADGE`, for what is typed | |
//! | `SETUP_CONSOLE` | console handle, for messages | |
//! | `SETUP_DONE` | | |

pub const SETUP_CONTROLLER: u64 = 100;
pub const SETUP_DMA: u64 = 101;
pub const SETUP_INPUT: u64 = 102;
pub const SETUP_CONSOLE: u64 = 103;
pub const SETUP_DONE: u64 = 104;
