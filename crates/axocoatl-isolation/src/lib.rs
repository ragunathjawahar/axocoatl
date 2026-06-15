pub mod error;
pub mod podman;
pub mod pty;
pub mod session_sandbox;

// Experimental, opt-in isolation tiers. The shipped boundary is the rootless
// Podman session sandbox above; these microVM / OCI tiers are gated out of the
// default build so it carries no unfinished isolation code.
#[cfg(feature = "firecracker-isolation")]
pub mod firecracker;
#[cfg(feature = "oci-isolation")]
pub mod oci_sandbox;
#[cfg(any(feature = "firecracker-isolation", feature = "oci-isolation"))]
pub mod tier;
#[cfg(feature = "firecracker-isolation")]
pub mod vsock;

#[cfg(feature = "wasmtime-sandbox")]
pub mod wasmtime_sandbox;

pub use error::*;
pub use session_sandbox::*;

#[cfg(feature = "firecracker-isolation")]
pub use firecracker::*;
#[cfg(feature = "oci-isolation")]
pub use oci_sandbox::*;
#[cfg(any(feature = "firecracker-isolation", feature = "oci-isolation"))]
pub use tier::*;
#[cfg(feature = "firecracker-isolation")]
pub use vsock::*;

#[cfg(feature = "wasmtime-sandbox")]
pub use wasmtime_sandbox::*;
