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

impl ServerBuilder {
    pub fn new() -> Self {
        Self {
            port: None,
            //TODO: Initialize fields
        }
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
            // TODO: Initialize additional fields
        })
    }
}

impl Default for ServerBuilder {
    fn default() -> Self {
        Self::new()
    }
}
