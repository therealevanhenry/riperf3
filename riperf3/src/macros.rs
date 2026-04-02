// riperf3/riperf3/src/macros.rs

#[doc(hidden)]
#[macro_export]
macro_rules! vprintln {
    ($($arg:tt)*) => {
        if $crate::utils::VERBOSE.load(std::sync::atomic::Ordering::Relaxed) {
            log::info!($($arg)*);
            println!($($arg)*);
        }
    };
}
