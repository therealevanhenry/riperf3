// riperf3/riperf3/src/server.rs
// Implements the Server types and functions for the riperf3 project.

use crate::error::ConfigError;
use crate::utils::DEFAULT_PORT;

// Server-specific struct
pub struct Server {
    port: u16,
    //TODO: Add fields
}

// Implement server-specific functions
impl Server {
    pub async fn run(&self) -> Result<(), ConfigError> {
        vprintln!("Server running on port: {}", self.port);
        //TODO: Implement server logic

        Ok(())
    }

    //TODO: Add additional functions
}

// Server builder struct
pub struct ServerBuilder {
    port: Option<u16>,
    //TODO: Add fields
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self {
            port: Some(DEFAULT_PORT),
            //TODO: Initialize fields
        }
    }
}

impl ServerBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn port(mut self, port: Option<u16>) -> Self {
        self.port = port;
        self
    }

    //TODO: Add methods for additional fields

    // Build function to produce a Server struct
    pub fn build(self) -> Result<Server, ConfigError> {
        // Validate required fields
        Ok(Server {
            // Initialize Client with validated fields
            // If there is no port, use DEFAULT_PORT
            port: self.port.unwrap_or(DEFAULT_PORT),
            //
            // TODO: Initialize additional fields
        })
    }
}

///////////////////////////////////////////////////////////////////////////////
// Unit tests for the server module ///////////////////////////////////////////
///////////////////////////////////////////////////////////////////////////////
#[cfg(test)]
mod tests {
    use super::*;

    // ServerBuilder tests
    mod server_builder_tests {
        use super::*;

        // Test default, new, and different fields
        #[test]
        fn test_server_builder_default() {
            let server_builder = ServerBuilder::default();
            assert_eq!(server_builder.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_server_builder_new() {
            let server_builder = ServerBuilder::new();
            assert_eq!(server_builder.port, Some(DEFAULT_PORT));
        }

        #[test]
        fn test_server_builder_port() {
            let server_builder = ServerBuilder::new().port(Some(1234));
            assert_eq!(server_builder.port, Some(1234));
        }

        //
        //TODO: Add additional tests for other fields

        // Test build
        #[test]
        fn test_server_builder_build() {
            // Test with default, this should work
            let server = ServerBuilder::default().build();
            assert!(server.is_ok());
            let server = server.unwrap();
            assert_eq!(server.port, DEFAULT_PORT);

            // Test with new, this should work
            let server = ServerBuilder::new().build();
            assert!(server.is_ok());
            let server = server.unwrap();
            assert_eq!(server.port, DEFAULT_PORT);

            // Test with adding a port, this should work
            let server = ServerBuilder::new().port(Some(1234)).build();
            assert!(server.is_ok());
            let server = server.unwrap();
            assert_eq!(server.port, 1234);
        }
    }

    // Server tests
    mod server_tests {
        use super::*;

        // Test defaults and setting different fields
        #[test]
        fn test_server_default() {
            let server = Server { port: DEFAULT_PORT };
            assert_eq!(server.port, DEFAULT_PORT);
        }

        #[test]
        fn test_server_port() {
            let server = Server { port: 1234 };
            assert_eq!(server.port, 1234);
        }

        // Test run
        #[tokio::test]
        async fn test_server_run() {
            let server = Server { port: 1234 };

            let result = server.run().await;
            assert!(result.is_ok());
        }
    }
}
