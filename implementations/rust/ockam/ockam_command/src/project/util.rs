use anyhow::Context as _;
use miette::miette;

use ockam_core::api::Request;
use tokio_retry::strategy::FixedInterval;
use tokio_retry::Retry;
use tracing::debug;

use ockam::identity::IdentityIdentifier;
use ockam::TcpTransport;
use ockam_api::cli_state::{StateDirTrait, StateItemTrait};
use ockam_api::cloud::project::Project;
use ockam_api::cloud::ORCHESTRATOR_AWAIT_TIMEOUT_MS;
use ockam_api::config::lookup::{LookupMeta, ProjectAuthority};
use ockam_api::multiaddr_to_addr;
use ockam_api::nodes::models::{self, secure_channel::*};
use ockam_core::compat::str::FromStr;
use ockam_core::flow_control::FlowControlId;
use ockam_multiaddr::{MultiAddr, Protocol};
use ockam_node::Context;

use crate::util::api::CloudOpts;
use crate::util::{api, RpcBuilder};
use crate::{CommandGlobalOpts, Result};

pub fn clean_projects_multiaddr(
    input: MultiAddr,
    projects_secure_channels: Vec<MultiAddr>,
) -> Result<MultiAddr> {
    let mut new_ma = MultiAddr::default();
    let mut sc_iter = projects_secure_channels.iter().peekable();
    for p in input.iter().peekable() {
        match p.code() {
            ockam_multiaddr::proto::Project::CODE => {
                let alias = p
                    .cast::<ockam_multiaddr::proto::Project>()
                    .ok_or_else(|| miette!("Invalid project value"))?;
                let sc = sc_iter
                    .next()
                    .ok_or_else(|| miette!("Missing secure channel for project {}", &*alias))?;
                for v in sc.iter().peekable() {
                    new_ma.push_back_value(&v)?;
                }
            }
            _ => new_ma.push_back_value(&p)?,
        }
    }
    debug!(%input, %new_ma, "Projects names replaced with secure channels");
    Ok(new_ma)
}

pub async fn get_projects_secure_channels_from_config_lookup(
    ctx: &Context,
    opts: &CommandGlobalOpts,
    meta: &LookupMeta,
    api_node: &str,
    tcp: Option<&TcpTransport>,
    credential_exchange_mode: CredentialExchangeMode,
) -> Result<Vec<MultiAddr>> {
    let mut sc = Vec::with_capacity(meta.project.len());

    // Create a secure channel for each project.
    for name in meta.project.iter() {
        // Get the project node's access route + identity id from the config
        let (project_access_route, project_identity_id) = {
            // This shouldn't fail, as we did a refresh above if we found any missing project.
            let p = opts
                .state
                .projects
                .get(name)
                .context(format!("Failed to get project {name} from config lookup"))?
                .config()
                .clone();
            let id = p
                .identity
                .context("Project should have identity set")?
                .to_string();
            let node_route =
                MultiAddr::from_str(&p.access_route).context("Invalid project node route")?;
            (node_route, id)
        };
        let (sc_address, _sc_flow_control_id) = create_secure_channel_to_project(
            ctx,
            opts,
            api_node,
            tcp,
            &project_access_route,
            &project_identity_id,
            credential_exchange_mode,
            None,
        )
        .await?;
        sc.push(sc_address);
    }

    // There should be the same number of project occurrences in the
    // input MultiAddr than there are in the secure channels vector.
    assert_eq!(meta.project.len(), sc.len());
    Ok(sc)
}

#[allow(clippy::too_many_arguments)]
pub async fn create_secure_channel_to_project(
    ctx: &ockam::Context,
    opts: &CommandGlobalOpts,
    api_node: &str,
    tcp: Option<&TcpTransport>,
    project_access_route: &MultiAddr,
    project_identity: &str,
    credential_exchange_mode: CredentialExchangeMode,
    identity: Option<String>,
) -> crate::Result<(MultiAddr, FlowControlId)> {
    let authorized_identifier = vec![IdentityIdentifier::from_str(project_identity)?];
    let mut rpc = RpcBuilder::new(ctx, opts, api_node).tcp(tcp)?.build();

    let payload = models::secure_channel::CreateSecureChannelRequest::new(
        project_access_route,
        Some(authorized_identifier),
        credential_exchange_mode,
        identity,
        None,
    );
    let req = Request::post("/node/secure_channel").body(payload);
    rpc.request(req).await?;

    let sc = rpc.parse_response::<CreateSecureChannelResponse>()?;
    Ok((sc.multiaddr()?, sc.flow_control_id()))
}

pub async fn create_secure_channel_to_authority(
    ctx: &ockam::Context,
    opts: &CommandGlobalOpts,
    node_name: &str,
    authority: IdentityIdentifier,
    addr: &MultiAddr,
    identity: Option<String>,
) -> crate::Result<(MultiAddr, FlowControlId)> {
    let mut rpc = RpcBuilder::new(ctx, opts, node_name).build();
    debug!(%addr, "establishing secure channel to project authority");
    let allowed = vec![authority];
    let payload = models::secure_channel::CreateSecureChannelRequest::new(
        addr,
        Some(allowed),
        CredentialExchangeMode::None,
        identity,
        None,
    );
    let req = Request::post("/node/secure_channel").body(payload);
    rpc.request(req).await?;
    let res = rpc.parse_response::<CreateSecureChannelResponse>()?;
    let addr = res.multiaddr()?;
    Ok((addr, res.flow_control_id()))
}

async fn delete_secure_channel<'a>(
    ctx: &ockam::Context,
    opts: &CommandGlobalOpts,
    api_node: &str,
    tcp: Option<&TcpTransport>,
    sc_addr: &MultiAddr,
) -> crate::Result<()> {
    let mut rpc = RpcBuilder::new(ctx, opts, api_node).tcp(tcp)?.build();
    let addr = multiaddr_to_addr(sc_addr).context("Failed to convert MultiAddr to addr")?;
    rpc.request(api::delete_secure_channel(&addr)).await?;
    rpc.is_ok()?;
    Ok(())
}

pub async fn check_project_readiness(
    ctx: &ockam::Context,
    opts: &CommandGlobalOpts,
    cloud_opts: &CloudOpts,
    api_node: &str,
    tcp: Option<&TcpTransport>,
    mut project: Project,
) -> Result<Project> {
    // Total of 10 Mins sleep strategy with 5 second intervals between each retry
    let retry_strategy =
        FixedInterval::from_millis(5000).take(ORCHESTRATOR_AWAIT_TIMEOUT_MS / 5000);

    // Persist project config prior to checking readiness which might take a while
    opts.state
        .projects
        .overwrite(&project.name, project.clone())?;

    let spinner_option = opts.terminal.progress_spinner();
    if let Some(spinner) = spinner_option.as_ref() {
        spinner.set_message("Waiting for project to be ready...");
    }

    // Check if Project and Project Authority info is available
    if !project.is_ready() {
        let cloud_route = &cloud_opts.route();
        let project_id = project.id.clone();
        project = Retry::spawn(retry_strategy.clone(), || async {
            let mut rpc = RpcBuilder::new(ctx, opts, api_node).build();

            // Handle the project show request result
            // so we can provide better errors in the case orchestrator does not respond timely
            if rpc
                .request(api::project::show(&project_id, cloud_route))
                .await
                .is_ok()
            {
                let p = rpc.parse_response::<Project>()?;
                if p.is_ready() {
                    return Ok(p.to_owned());
                }
            }
            Err(miette!("Project creation timed out. Plaese try again."))
        })
        .await?;
    }

    {
        if let Some(spinner) = spinner_option.as_ref() {
            spinner.set_message("Establishing connection to the project...");
        }

        Retry::spawn(retry_strategy.clone(), || async {
            // Handle the reachable result, so we can provide better errors in the case a project isn't
            if let Ok(reachable) = project.is_reachable().await {
                if reachable {
                    return Ok(());
                }
            }

            Err(miette!("Timed out while trying to establish a connection to the project. Please try again."))
        }).await?;
    }

    {
        if let Some(spinner) = spinner_option.as_ref() {
            spinner.set_message("Establishing secure channel to project...");
        }

        let project_route = project.access_route()?;
        let project_identity = project
            .identity
            .as_ref()
            .context("Project identity is not set.")?
            .to_string();

        Retry::spawn(retry_strategy.clone(), || async {
            if let Ok((sc_addr, _sc_flow_control_id)) = create_secure_channel_to_project(
                ctx,
                opts,
                api_node,
                tcp,
                &project_route,
                &project_identity,
                CredentialExchangeMode::None,
                None,
            )
            .await
            {
                // Try to delete secure channel, ignore result.
                let _ = delete_secure_channel(ctx, opts, api_node, tcp, &sc_addr).await;
                return Ok(());
            }

            Err(miette!("Timed out while trying to establish a secure channel to the project. Please try again."))
        })
        .await?;
    }

    {
        if let Some(spinner) = spinner_option.as_ref() {
            spinner.set_message("Establishing secure channel to project authority...");
        }

        let authority = ProjectAuthority::from_raw(
            &project.authority_access_route,
            &project.authority_identity,
        )
        .await?
        .context("Project does not have an authority defined.")?;

        Retry::spawn(retry_strategy.clone(), || async {
            if let Ok((sc_addr, _)) = create_secure_channel_to_authority(
                ctx,
                opts,
                api_node,
                authority.identity_id().clone(),
                authority.address(),
                None,
            )
            .await
            {
                // Try to delete secure channel, ignore result.
                let _ = delete_secure_channel(ctx, opts, api_node, tcp, &sc_addr).await;
                return Ok(());
            }

            Err(miette!("Timed out while trying to establish a secure channel to the project authority. Please try again."))
        })
        .await?;
    }

    if let Some(spinner) = spinner_option.as_ref() {
        spinner.finish_and_clear();
    }

    // Persist project config with all its fields
    opts.state
        .projects
        .overwrite(&project.name, project.clone())?;
    Ok(project)
}

pub async fn refresh_projects(
    ctx: &Context,
    opts: &CommandGlobalOpts,
    api_node: &str,
    controller_route: &MultiAddr,
    tcp: Option<&TcpTransport>,
) -> Result<()> {
    let mut rpc = RpcBuilder::new(ctx, opts, api_node).tcp(tcp)?.build();
    rpc.request(api::project::list(controller_route)).await?;
    let projects = rpc.parse_response::<Vec<Project>>()?;
    for project in projects {
        opts.state
            .projects
            .overwrite(&project.name, project.clone())?;
    }
    Ok(())
}
