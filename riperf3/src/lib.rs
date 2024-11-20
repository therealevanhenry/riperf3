pub mod iperf_api;

// Verbose mode println!(...) macro
#[macro_export]
macro_rules! vprintln {
    ($verbose:expr, $($arg:tt)*) => {
        if $verbose {
            println!($($arg)*);
        }
    };
}

pub fn run_client() {
    println!("Running client");
}

pub fn run_server() {
    println!("Running server");
}
