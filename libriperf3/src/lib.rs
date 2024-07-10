pub fn perform_test() {
    println!("Hello, world!");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        // Simply invoke the function to ensure it works
        perform_test();
    }
}
