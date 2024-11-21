pub mod iperf_api;

// Verbose mode println!(...) macro
#[macro_export]
macro_rules! vprintln {
    ($verbose:expr, $($arg:tt)*) => {
        if $verbose {
            log::trace!($($arg)*);
            println!($($arg)*);
        }
    };
}

pub fn run_client() {
    vprintln!(true, "Running client");
    log::debug!("test client");
}

pub fn run_server() {
    vprintln!(true, "Running server");
    log::debug!("test server");
}
