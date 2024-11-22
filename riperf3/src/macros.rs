// riperf3/riperf3/src/macros.rs

#[macro_export]
macro_rules! vprintln {
    ($($arg:tt)*) => {
        if $crate::utils::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            log::trace!($($arg)*);
            println!($($arg)*);
        }
    };
}
