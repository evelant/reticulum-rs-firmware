//! Product application sessions over the process-wide PRNS node.

use std::sync::Arc;

use personal_rns::wire::DestinationHash;
use reticulum_appliance_runtime::{
    ApplianceCapabilitiesView, ConnectFailure, ConnectedSession, ConnectionMetadata,
    ConnectionTransport, Connector, NearbyPeerView,
};
use reticulum_appliance_sync::{DeviceApiRequestSession, DeviceApiRequester};
use reticulum_device_api::{DeviceRequest, DeviceResponse, MANAGEMENT_REQUEST_PATH};

use crate::prns_node::NativePrnsNode;

/// Finds one enrolled appliance through an app-owned PRNS node.
///
/// Native mobile and host-service entry points share this product adapter.
/// PRNS remains the sole owner of Reticulum routes, Links, requests, and
/// packet interfaces; this connector only selects one exact management
/// application and presents its typed operations to the durable app runtime.
pub struct PrnsConnector {
    node: Arc<NativePrnsNode>,
    destination: Option<DestinationHash>,
}

impl PrnsConnector {
    /// Select one exact management destination, or scan accepted announce
    /// candidates while an application is still unconfigured.
    #[must_use]
    pub fn new(node: Arc<NativePrnsNode>, destination: Option<[u8; 16]>) -> Self {
        Self {
            node,
            destination: destination.map(DestinationHash::new),
        }
    }
}

impl Connector for PrnsConnector {
    fn connect(&mut self) -> Result<ConnectedSession, ConnectFailure> {
        log::debug!(
            "appliance connector: starting connection attempt for configured destination {}",
            self.destination
                .map(|destination| hex::encode(destination.as_bytes()))
                .unwrap_or_else(|| "<discovery>".to_owned())
        );
        if !self.node.is_running() {
            log::warn!("appliance connector: PRNS node is not running");
            return Err(ConnectFailure::retryable(
                "the client PRNS node is not running",
            ));
        }

        let candidates = self
            .destination
            .map(|destination| vec![destination])
            .unwrap_or_else(|| self.node.management_destination_hashes());
        if candidates.is_empty() {
            return Err(ConnectFailure::retryable(
                "no appliance management announce has been observed",
            ));
        }

        let mut last_failure = None;
        for destination in candidates {
            let destination_hex = hex::encode(destination.as_bytes());
            log::debug!(
                "appliance connector: requesting authorized identity from {destination_hex}"
            );
            let response = self.node.request_device_api_blocking(
                destination,
                MANAGEMENT_REQUEST_PATH,
                DeviceRequest::IdentitySummary,
            );
            match response {
                Ok(DeviceResponse::IdentitySummary(summary))
                    if summary.primary_destination().0 == *destination.as_bytes()
                        && summary.lxmf_delivery_destination().is_some() =>
                {
                    log::info!(
                        "appliance connector: authorized identity request succeeded for {destination_hex}"
                    );
                    let capabilities = match self.node.request_device_api_blocking(
                        destination,
                        MANAGEMENT_REQUEST_PATH,
                        DeviceRequest::SystemCapabilities,
                    ) {
                        Ok(DeviceResponse::SystemCapabilities(capabilities)) => capabilities,
                        Ok(DeviceResponse::Error(error)) => {
                            log::warn!(
                                "appliance connector: {destination_hex} rejected the capability request with {:?}",
                                error.code
                            );
                            last_failure = Some(format!(
                                "candidate rejected the capability request with {:?}",
                                error.code
                            ));
                            continue;
                        }
                        Ok(response) => {
                            log::warn!(
                                "appliance connector: {destination_hex} returned unexpected capability response kind {}",
                                response.kind()
                            );
                            last_failure = Some(format!(
                                "candidate returned unexpected capability response kind {}",
                                response.kind()
                            ));
                            continue;
                        }
                        Err(error) => {
                            log::warn!(
                                "appliance connector: capability request to {destination_hex} failed: {error}"
                            );
                            last_failure = Some(error.to_string());
                            continue;
                        }
                    };
                    return Ok(ConnectedSession::new(
                        DeviceApiRequestSession::new(PrnsRequester {
                            node: Arc::clone(&self.node),
                            destination,
                        }),
                        ConnectionMetadata::new(
                            ConnectionTransport::Reticulum,
                            destination_hex.clone(),
                            destination_hex,
                        ),
                    )
                    .with_capabilities(
                        ApplianceCapabilitiesView::from_device_api(capabilities)
                            .with_local_nearby_peers(),
                    ));
                }
                Ok(DeviceResponse::Error(error)) => {
                    log::warn!(
                        "appliance connector: {destination_hex} rejected the authorized identity request with {:?}",
                        error.code
                    );
                    last_failure = Some(format!(
                        "candidate rejected the authorized identity request with {:?}",
                        error.code
                    ));
                }
                Ok(response) => {
                    log::warn!(
                        "appliance connector: {destination_hex} returned unexpected response kind {}",
                        response.kind()
                    );
                    last_failure = Some(format!(
                        "candidate returned unexpected response kind {}",
                        response.kind()
                    ));
                }
                Err(error) => {
                    log::warn!(
                        "appliance connector: authorized identity request to {destination_hex} failed: {error}"
                    );
                    last_failure = Some(error.to_string());
                }
            }
        }

        Err(ConnectFailure::retryable(last_failure.unwrap_or_else(
            || "no announced appliance accepted this enrolled Reticulum identity".to_owned(),
        )))
    }

    fn nearby_peers(&self) -> Result<Vec<NearbyPeerView>, String> {
        if !self.node.is_running() {
            return Err("the client PRNS node is not running".to_owned());
        }
        Ok(self.node.nearby_peers())
    }
}

struct PrnsRequester {
    node: Arc<NativePrnsNode>,
    destination: DestinationHash,
}

impl DeviceApiRequester for PrnsRequester {
    fn appliance_id(&self) -> [u8; 16] {
        *self.destination.as_bytes()
    }

    fn request(&mut self, request: DeviceRequest<'_>) -> Result<DeviceResponse, String> {
        self.node
            .request_device_api_blocking(self.destination, MANAGEMENT_REQUEST_PATH, request)
            .map_err(|error| error.to_string())
    }

    fn is_usable(&self) -> bool {
        self.node.is_running()
    }
}
