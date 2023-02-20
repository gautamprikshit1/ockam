use crate::alloc::string::ToString;
use ockam_core::compat::boxed::Box;
use ockam_core::compat::collections::HashMap;
use ockam_core::compat::string::String;
use ockam_core::errcode::{Kind, Origin};
use ockam_core::{AsyncTryClone, Error, Message, Result, Route, Routed, Worker};

use crate::authenticated_storage::mem::InMemoryStorage;
use ockam_node::Context;
use ockam_vault::Vault;
use CredentialIssuerRequest::*;
use CredentialIssuerResponse::*;

use crate::credential::Credential;
use crate::{Identity, IdentityIdentifier, PublicIdentity};
use serde::{Deserialize, Serialize};

/// This struct provides a simplified credential issuer which can be used in test scenarios
/// by starting it as a Worker on any given node.
///
/// Note that the storage associated to the issuer identity will not persist between runs.
pub struct CredentialIssuer {
    identity: Identity<Vault, InMemoryStorage>,
}

/// This trait provides an interface for a CredentialIssuer so that it can be called directly
/// or via a worker by sending messages
#[ockam_core::async_trait]
pub trait CredentialIssuerApi {
    /// Return the issuer public identity
    async fn public_identity(&self) -> Result<PublicIdentity>;

    /// Create an authenticated credential for an attribute name/value pair
    /// and a given subject
    async fn get_attribute_credential(
        &self,
        subject: &IdentityIdentifier,
        attribute_name: &str,
        attribute_value: &str,
    ) -> Result<Credential>;

    /// Create an authenticated credential for a list of attribute name/value pairs
    /// and a given subject
    async fn get_credential(
        &self,
        subject: &IdentityIdentifier,
        attributes: HashMap<String, String>,
    ) -> Result<Credential>;
}

impl CredentialIssuer {
    /// Create a fully in-memory issuer for testing
    pub async fn create(ctx: &Context) -> Result<CredentialIssuer> {
        let identity = Identity::create(ctx, &Vault::create()).await?;
        Ok(CredentialIssuer { identity })
    }

    /// Create a new CredentialIssuer from an Identity
    pub fn new(identity: Identity<Vault, InMemoryStorage>) -> CredentialIssuer {
        CredentialIssuer { identity }
    }
}

#[ockam_core::async_trait]
impl CredentialIssuerApi for CredentialIssuer {
    /// Return the issuer public identity
    async fn public_identity(&self) -> Result<PublicIdentity> {
        self.identity.to_public().await
    }

    /// Create an authenticated credential for an attribute name/value pair
    async fn get_attribute_credential(
        &self,
        subject: &IdentityIdentifier,
        attribute_name: &str,
        attribute_value: &str,
    ) -> Result<Credential> {
        let mut attributes = HashMap::new();
        attributes.insert(attribute_name.into(), attribute_value.into());
        self.get_credential(subject, attributes).await
    }

    /// Create an authenticated credential for a list of attribute name/value pairs
    async fn get_credential(
        &self,
        subject: &IdentityIdentifier,
        attributes: HashMap<String, String>,
    ) -> Result<Credential> {
        let mut builder = Credential::builder(subject.clone());
        builder = attributes
            .iter()
            .fold(builder, |b, (attribute_name, attribute_value)| {
                b.with_attribute(attribute_name, attribute_value.as_bytes())
            });
        let credential = self.identity.issue_credential(builder).await?;
        Ok(credential)
    }
}

/// Worker implementation for a CredentialIssuer
/// This worker provides an API to the CredentialIssuer in order to:
///   - get a credential
///   - get the issuer public identity in order to verify credentials locally
#[ockam_core::worker]
impl Worker for CredentialIssuer {
    type Message = CredentialIssuerRequest;
    type Context = Context;

    async fn handle_message(
        &mut self,
        ctx: &mut Context,
        msg: Routed<CredentialIssuerRequest>,
    ) -> Result<()> {
        let return_route = msg.return_route();
        match msg.body() {
            GetAttributeCredential(subject, name, value) => {
                let credential = self
                    .get_attribute_credential(&subject, name.as_str(), value.as_str())
                    .await?;
                ctx.send(return_route, CredentialResponse(credential)).await
            }
            GetCredential(subject, attributes) => {
                let credential = self.get_credential(&subject, attributes).await?;
                ctx.send(return_route, CredentialResponse(credential)).await
            }
            GetPublicIdentity => {
                let identity = self.public_identity().await?;
                ctx.send(return_route, PublicIdentityResponse(identity))
                    .await
            }
        }
    }
}

/// Requests for the CredentialIssuer worker API
#[derive(ockam_core::Message, Serialize, Deserialize)]
pub enum CredentialIssuerRequest {
    /// get an authenticated credential for a subject and an attribute name/value
    GetAttributeCredential(IdentityIdentifier, String, String),
    /// get an authenticated credential a subject and a list of attributes
    GetCredential(IdentityIdentifier, HashMap<String, String>),
    /// get the public identity of the issuer
    GetPublicIdentity,
}

/// Responses for the CredentialIssuer worker API
#[derive(ockam_core::Message, Serialize, Deserialize)]
pub enum CredentialIssuerResponse {
    /// return an authenticated credential
    CredentialResponse(Credential),
    /// return the public identity of the issuer
    PublicIdentityResponse(PublicIdentity),
}

/// Client access to an CredentialIssuer worker
pub struct CredentialIssuerClient {
    ctx: Context,
    credential_issuer_route: Route,
}

impl CredentialIssuerClient {
    /// Create an access to an CredentialIssuer worker given a route to that worker
    /// It uses a Context to send and receive messages
    pub async fn new(ctx: &Context, issuer_route: Route) -> Result<CredentialIssuerClient> {
        Ok(CredentialIssuerClient {
            ctx: ctx.async_try_clone().await?,
            credential_issuer_route: issuer_route,
        })
    }
}

#[ockam_core::async_trait]
impl CredentialIssuerApi for CredentialIssuerClient {
    async fn public_identity(&self) -> Result<PublicIdentity> {
        let response = self
            .ctx
            .send_and_receive(self.credential_issuer_route.clone(), GetPublicIdentity)
            .await?;
        match response {
            PublicIdentityResponse(identity) => Ok(identity),
            _ => Err(error("missing public identity for the credential issuer")),
        }
    }

    async fn get_attribute_credential(
        &self,
        subject: &IdentityIdentifier,
        attribute_name: &str,
        attribute_value: &str,
    ) -> Result<Credential> {
        let response = self
            .ctx
            .send_and_receive(
                self.credential_issuer_route.clone(),
                GetAttributeCredential(
                    subject.clone(),
                    attribute_name.into(),
                    attribute_value.into(),
                ),
            )
            .await?;
        match response {
            CredentialResponse(credential) => Ok(credential),
            _ => Err(error("missing credential")),
        }
    }

    async fn get_credential(
        &self,
        subject: &IdentityIdentifier,
        attributes: HashMap<String, String>,
    ) -> Result<Credential> {
        let response = self
            .ctx
            .send_and_receive(
                self.credential_issuer_route.clone(),
                GetCredential(subject.clone(), attributes),
            )
            .await?;
        match response {
            CredentialResponse(credential) => Ok(credential),
            _ => Err(error("missing credential")),
        }
    }
}

fn error(message: &str) -> Error {
    Error::new(Origin::Application, Kind::Invalid, message.to_string())
}