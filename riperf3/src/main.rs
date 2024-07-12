extern crate libriperf3;

use libriperf3::iperf_api;

fn main() {
    let test = iperf_api::IperfTest {
        ..iperf_api::IperfTest::default()
    };

    println!("{:?}", test);
}
