// riperf3/riperf3/src/utils.rs

use std::sync::atomic::{AtomicBool, Ordering};

// Use an AtomicBool to store the verbose flag to ensure thread safety
pub static VERBOSE: AtomicBool = AtomicBool::new(false);

// Provide a setter function for the verbose flag
pub fn set_verbose(verbose: bool) {
    VERBOSE.store(verbose, Ordering::Relaxed);
}

// Default values for riperf3
pub const DEFAULT_PORT: u16 = 5201; // default port number is 5201
pub const DEFAULT_OMIT: u32 = 0; // default omit period is 0 seconds
pub const DEFAULT_DURATION: u32 = 10; // default duration is 10 seconds
pub const DEFAULT_TIMESTAMP_FORMAT: &str = "%c "; // default timestamp format
