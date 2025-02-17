// riperf3/riperf3/src/client.rs
// Implements the Client types and functions for the riperf3 project.

use crate::error::ConfigError;
use crate::utils::DEFAULT_PORT;

// Client-specific struct
#[derive(Debug)]
pub struct Client {
    host: String,
    port: u16,
    //TODO: Add fields
}

// Implement client-specific functions
impl Client {
    pub async fn run(&self) -> Result<(), ConfigError> {
        vprintln!("Client connecting to: {}:{}", self.host, self.port);
        //TODO: Implement client logic

        Ok(())
    }

    //TODO: Add additional functions
}

// Client builder struct
pub struct ClientBuilder {
    host: Option<String>,
    port: Option<u16>,
    //TODO: Add fields
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            host: None,
            port: Some(DEFAULT_PORT),
            //TODO: Initialize fields
        }
    }
}

impl ClientBuilder {
    pub fn new(host: Option<String>) -> Self {
        Self::default().host(host)
    }

    pub fn host(mut self, host: Option<String>) -> Self {
        self.host = host;
        self
    }

    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    //TODO: Add methods for additional fields

    // Build function to produce a Client struct
    pub fn build(self) -> Result<Client, ConfigError> {
        // Validate required fields
        Ok(Client {
            // Initialize Client with validated fields
            host: self.host.ok_or(ConfigError::MissingField("host"))?,

            // If there is no port, use DEFAULT_PORT
            port: self.port.unwrap_or(DEFAULT_PORT),
            //
            // TODO: Initialize additional fields
        })
    }
}

//////////////////////////////////////////////////////////////////////////////
// Unit tests for the client module //////////////////////////////////////////
//////////////////////////////////////////////////////////////////////////////
#[cfg(test)]
mod tests {
    use super::*;

    // ClientBuilder tests
    mod client_builder_tests {
        use super::*;

        // Test default, new, and different fields
        #[test]
        fn test_client_builder_default() {
            let client_builder = ClientBuilder::default();
            assert_eq!(client_builder.host, None);
            assert_eq!(client_builder.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_client_builder_new() {
            let client_builder = ClientBuilder::new(Some("localhost".to_string()));
            assert_eq!(client_builder.host, Some("localhost".to_string()));
            assert_eq!(client_builder.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_client_builder_host() {
            let client_builder = ClientBuilder::new(Some("localhost".to_string())).host(Some("otherhost".to_string()));
            assert_eq!(client_builder.host, Some("otherhost".to_string()));
        }

        #[test]
        fn test_client_builder_port() {
            let client_builder = ClientBuilder::new(Some("localhost".to_string())).port(Some(1234));
            assert_eq!(client_builder.port, Some(1234));
        }

        //
        //TODO: Add tests for additional fields

        // Test build
        #[test]
        fn test_client_builder_build() {
            // Test with default values, this should return a ConfigError::MissingField
            let client = ClientBuilder::default().build();
            assert!(client.is_err());
            assert_eq!(client.unwrap_err(), ConfigError::MissingField("host"));

            // Test new, this should work
            let client = ClientBuilder::new(Some("localhost".to_string())).build();
            assert!(client.is_ok());
            let client = client.unwrap();
            assert_eq!(client.host, "localhost");
            assert_eq!(client.port, DEFAULT_PORT);

            // Test with new and change the host value, this should work
            let client = ClientBuilder::new(Some("localhost".to_string())).host(Some("otherhost".to_string())).build();
            assert!(client.is_ok());
            let client = client.unwrap();
            assert_eq!(client.host, "otherhost");
            assert_eq!(client.port, DEFAULT_PORT);

            // Test with new and set the port value, this should work
            let client = ClientBuilder::new(Some("localhost".to_string())).port(Some(1234)).build();
            assert!(client.is_ok());
            let client = client.unwrap();
            assert_eq!(client.host, "localhost");
            assert_eq!(client.port, 1234);

            // Test with new and set both host and port values, this should work
            let client = ClientBuilder::new(Some("localhost".to_string()))
                .host(Some("otherhost".to_string()))
                .port(Some(1234))
                .build();
            assert!(client.is_ok());
            let client = client.unwrap();
            assert_eq!(client.host, "otherhost");
            assert_eq!(client.port, 1234);
        }
    }

    // Client tests
    mod client_tests {
        use super::*;

        // Test defaults and setting different fields
        #[test]
        fn test_client_default() {
            let client = Client {
                host: "localhost".to_string(),
                port: DEFAULT_PORT,
            };
            assert_eq!(client.host, "localhost");
            assert_eq!(client.port, DEFAULT_PORT);
        }

        #[test]
        fn test_client_with_fields() {
            let client = Client {
                host: "localhost".to_string(),
                port: 1234,
            };
            assert_eq!(client.host, "localhost");
            assert_eq!(client.port, 1234);
        }

        // Test run
        #[tokio::test]
        async fn test_client_run() {
            let client = Client {
                host: "localhost".to_string(),
                port: DEFAULT_PORT,
            };
            let result = client.run().await;
            assert!(result.is_ok());
        }
    }
}
