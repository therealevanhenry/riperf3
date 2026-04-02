// riperf3/riperf3/src/macros.rs

#[doc(hidden)]
#[macro_export]
macro_rules! vprintln {
    ($($arg:tt)*) => {
        {
            log::info!($($arg)*);
            println!($($arg)*);
        }
    };
}
