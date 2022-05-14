// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use aptos_rest_client::Client as RestClient;
use aptos_sdk::{move_types::account_address::AccountAddress, transaction_builder::aptos_stdlib};
use forge::{ForgeConfig, Options, Result, *};
use std::{env, num::NonZeroUsize, process, time::Duration};
use structopt::StructOpt;
use testcases::{
    compatibility_test::SimpleValidatorUpgrade, fixed_tps_test::FixedTpsTest,
    gas_price_test::NonZeroGasPrice, generate_traffic, partial_nodes_down_test::PartialNodesDown,
    performance_test::PerformanceBenchmark, reconfiguration_test::ReconfigurationTest,
    state_sync_performance::StateSyncPerformance,
};
use tokio::runtime::Runtime;
use url::Url;

#[derive(StructOpt, Debug)]
struct Args {
    // general options
    #[structopt(long, default_value = "15")]
    accounts_per_client: usize,
    #[structopt(long)]
    workers_per_ac: Option<usize>,
    #[structopt(long, default_value = "0")]
    wait_millis: u64,
    #[structopt(long)]
    burst: bool,
    #[structopt(flatten)]
    options: Options,
    #[structopt(long, help = "Specify a test suite to run")]
    suite: Option<String>,
    #[structopt(long, multiple = true)]
    changelog: Option<Vec<String>>,

    // subcommand groups
    #[structopt(flatten)]
    cli_cmd: CliCommand,
}

#[derive(StructOpt, Debug)]
enum CliCommand {
    Test(TestCommand),
    Operator(OperatorCommand),
}

#[derive(StructOpt, Debug)]
enum TestCommand {
    LocalSwarm(LocalSwarm),
    K8sSwarm(K8sSwarm),
}

#[derive(StructOpt, Debug)]
enum OperatorCommand {
    SetValidator(SetValidator),
    CleanUp(CleanUp),
    Resize(Resize),
}

#[derive(StructOpt, Debug)]
struct LocalSwarm {}

#[derive(StructOpt, Debug)]
struct K8sSwarm {
    #[structopt(
        long,
        help = "Override the helm repo used for k8s tests",
        default_value = "testnet-internal"
    )]
    helm_repo: String,
    #[structopt(
        long,
        help = "The image tag currently is used for validators",
        default_value = "devnet"
    )]
    image_tag: String,
    #[structopt(
        long,
        help = "Image tag for validator software to do backward compatibility test",
        default_value = "devnet"
    )]
    base_image_tag: String,
    #[structopt(long, help = "Name of the EKS cluster")]
    cluster_name: String,
    #[structopt(
        long,
        help = "Path to flattened directory containing compiled Move modules"
    )]
    move_modules_dir: Option<String>,
}

#[derive(StructOpt, Debug)]
struct SetValidator {
    validator_name: String,
    #[structopt(long, help = "Override the image tag used for upgrade validators")]
    image_tag: String,
    #[structopt(
        long,
        help = "Override the helm repo used for k8s tests",
        default_value = "testnet-internal"
    )]
    helm_repo: String,
}

#[derive(StructOpt, Debug)]
struct CleanUp {
    #[structopt(long, help = "If set, uses k8s service account to auth with AWS")]
    auth_with_k8s_env: bool,
    #[structopt(long, help = "Name of the EKS cluster")]
    cluster_name: String,
}

#[derive(StructOpt, Debug)]
struct Resize {
    #[structopt(long, default_value = "30")]
    num_validators: usize,
    #[structopt(
        long,
        help = "Override the image tag used for validators",
        default_value = "devnet"
    )]
    validator_image_tag: String,
    #[structopt(
        long,
        help = "Override the image tag used for testnet-specific components",
        default_value = "devnet"
    )]
    testnet_image_tag: String,
    #[structopt(
        long,
        help = "If set, performs validator healthcheck and assumes k8s DNS access"
    )]
    require_validator_healthcheck: bool,
    #[structopt(long, help = "If set, uses k8s service account to auth with AWS")]
    auth_with_k8s_env: bool,
    #[structopt(
        long,
        help = "Override the helm repo used for k8s tests",
        default_value = "testnet-internal"
    )]
    helm_repo: String,
    #[structopt(long, help = "Name of the EKS cluster")]
    cluster_name: String,
    #[structopt(
        long,
        help = "Path to flattened directory containing compiled Move modules"
    )]
    move_modules_dir: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::from_args();
    let mut global_emit_job_request = EmitJobRequest::default()
        .accounts_per_client(args.accounts_per_client)
        .thread_params(EmitThreadParams {
            wait_millis: args.wait_millis,
            wait_committed: !args.burst,
        });
    if let Some(workers_per_endpoint) = args.workers_per_ac {
        global_emit_job_request =
            global_emit_job_request.workers_per_endpoint(workers_per_endpoint);
    }

    let runtime = Runtime::new()?;
    match args.cli_cmd {
        // cmd input for test
        CliCommand::Test(test_cmd) => match test_cmd {
            TestCommand::LocalSwarm(..) => run_forge(
                local_test_suite(),
                LocalFactory::from_workspace()?,
                &args.options,
                args.changelog,
                global_emit_job_request,
            ),
            TestCommand::K8sSwarm(k8s) => {
                let mut test_suite = k8s_test_suite();
                if let Some(suite) = args.suite.as_ref() {
                    test_suite = get_test_suite(suite);
                }
                if let Some(move_modules_dir) = k8s.move_modules_dir {
                    test_suite = test_suite.with_genesis_modules_path(move_modules_dir);
                }
                run_forge(
                    test_suite,
                    K8sFactory::new(
                        k8s.cluster_name,
                        k8s.helm_repo,
                        k8s.image_tag,
                        k8s.base_image_tag,
                    )
                    .unwrap(),
                    &args.options,
                    args.changelog,
                    global_emit_job_request,
                )
            }
        },
        // cmd input for cluster operations
        CliCommand::Operator(op_cmd) => match op_cmd {
            OperatorCommand::SetValidator(set_validator) => set_validator_image_tag(
                &set_validator.validator_name,
                &set_validator.image_tag,
                &set_validator.helm_repo,
            ),
            OperatorCommand::CleanUp(cleanup) => {
                uninstall_from_k8s_cluster()?;
                runtime.block_on(set_eks_nodegroup_size(
                    cleanup.cluster_name,
                    0,
                    cleanup.auth_with_k8s_env,
                ))
            }
            OperatorCommand::Resize(resize) => {
                runtime.block_on(set_eks_nodegroup_size(
                    resize.cluster_name,
                    resize.num_validators,
                    resize.auth_with_k8s_env,
                ))?;
                uninstall_from_k8s_cluster()?;
                runtime.block_on(clean_k8s_cluster(
                    resize.helm_repo,
                    resize.num_validators,
                    resize.validator_image_tag,
                    resize.testnet_image_tag,
                    resize.require_validator_healthcheck,
                    resize.move_modules_dir,
                ))?;
                Ok(())
            }
        },
    }
}

pub fn run_forge<F: Factory>(
    tests: ForgeConfig<'_>,
    factory: F,
    options: &Options,
    logs: Option<Vec<String>>,
    global_job_request: EmitJobRequest,
) -> Result<()> {
    let forge = Forge::new(options, tests, factory, global_job_request);

    if options.list {
        forge.list()?;

        return Ok(());
    }

    match forge.run() {
        Ok(report) => {
            if let Some(mut changelog) = logs {
                if changelog.len() != 2 {
                    println!("Use: changelog <from> <to>");
                    process::exit(1);
                }
                let to_commit = changelog.remove(1);
                let from_commit = Some(changelog.remove(0));
                send_changelog_message(&report.to_string(), &from_commit, &to_commit);
            }
            Ok(())
        }
        Err(e) => {
            eprintln!("Failed to run tests:\n{}", e);
            Err(e)
        }
    }
}

pub fn send_changelog_message(perf_msg: &str, from_commit: &Option<String>, to_commit: &str) {
    println!(
        "Generating changelog from {:?} to {}",
        from_commit, to_commit
    );
    let changelog = get_changelog(from_commit.as_ref(), to_commit);
    let msg = format!("{}\n\n{}", changelog, perf_msg);
    let slack_url: Option<Url> = env::var("SLACK_URL")
        .map(|u| u.parse().expect("Failed to parse SLACK_URL"))
        .ok();
    if let Some(ref slack_url) = slack_url {
        let slack_client = SlackClient::new();
        if let Err(e) = slack_client.send_message(slack_url, &msg) {
            println!("Failed to send slack message: {}", e);
        }
    }
}

fn get_changelog(prev_commit: Option<&String>, upstream_commit: &str) -> String {
    let github_client = GitHub::new();
    let commits = github_client.get_commits("aptos-labs/aptos-core", upstream_commit);
    match commits {
        Err(e) => {
            println!("Failed to get github commits: {:?}", e);
            format!("*Revision upstream_{}*", upstream_commit)
        }
        Ok(commits) => {
            let mut msg = format!("*Revision {}*", upstream_commit);
            for commit in commits {
                if let Some(prev_commit) = prev_commit {
                    if commit.sha.starts_with(prev_commit) {
                        break;
                    }
                }
                let commit_lines: Vec<_> = commit.commit.message.split('\n').collect();
                let commit_head = commit_lines[0];
                let commit_head = commit_head.replace("[breaking]", "*[breaking]*");
                let short_sha = &commit.sha[..6];
                let email_parts: Vec<_> = commit.commit.author.email.split('@').collect();
                let author = email_parts[0];
                let line = format!("\n>\u{2022} {} _{}_ {}", short_sha, author, commit_head);
                msg.push_str(&line);
            }
            msg
        }
    }
}

fn get_test_suite(suite_name: &str) -> ForgeConfig<'static> {
    match suite_name {
        "land_blocking_compat" => land_blocking_test_compat_suite(),
        "land_blocking" => land_blocking_test_suite(),
        "pre_release" => pre_release_suite(),
        single_test => single_test_suite(single_test),
    }
}

fn local_test_suite() -> ForgeConfig<'static> {
    ForgeConfig::default()
        .with_aptos_tests(&[&FundAccount, &TransferCoins])
        .with_admin_tests(&[&GetMetadata])
        .with_network_tests(&[&RestartValidator, &EmitTransaction])
        .with_genesis_modules_bytes(cached_framework_packages::module_blobs().to_vec())
}

fn k8s_test_suite() -> ForgeConfig<'static> {
    ForgeConfig::default()
        .with_initial_validator_count(NonZeroUsize::new(30).unwrap())
        .with_aptos_tests(&[&FundAccount, &TransferCoins])
        .with_admin_tests(&[&GetMetadata])
        .with_network_tests(&[&EmitTransaction, &SimpleValidatorUpgrade])
}

fn single_test_suite(test_name: &str) -> ForgeConfig<'static> {
    let config =
        ForgeConfig::default().with_initial_validator_count(NonZeroUsize::new(30).unwrap());
    match test_name {
        "bench" => config.with_network_tests(&[&PerformanceBenchmark]),
        "state_sync" => config.with_network_tests(&[&StateSyncPerformance]),
        "compat" => config.with_network_tests(&[&SimpleValidatorUpgrade]),
        "config" => config.with_network_tests(&[&ReconfigurationTest]),
        _ => config.with_network_tests(&[&PerformanceBenchmark]),
    }
}

fn land_blocking_test_suite() -> ForgeConfig<'static> {
    ForgeConfig::default()
        .with_initial_validator_count(NonZeroUsize::new(30).unwrap())
        .with_network_tests(&[&PerformanceBenchmark])
}

fn land_blocking_test_compat_suite() -> ForgeConfig<'static> {
    // please keep tests order in this suite
    // since later tests node version rely on first test
    ForgeConfig::default()
        .with_initial_validator_count(NonZeroUsize::new(30).unwrap())
        .with_network_tests(&[&SimpleValidatorUpgrade, &PerformanceBenchmark])
        .with_initial_version(InitialVersion::Oldest)
}

fn pre_release_suite() -> ForgeConfig<'static> {
    // please keep tests order in this suite
    // since later tests node version rely on first test
    ForgeConfig::default()
        .with_initial_validator_count(NonZeroUsize::new(30).unwrap())
        .with_network_tests(&[
            &FixedTpsTest,
            &PerformanceBenchmark,
            &NonZeroGasPrice,
            &PartialNodesDown,
            &ReconfigurationTest,
            &StateSyncPerformance,
        ])
}

//TODO Make public test later
#[derive(Debug)]
struct GetMetadata;

impl Test for GetMetadata {
    fn name(&self) -> &'static str {
        "get_metadata"
    }
}

impl AdminTest for GetMetadata {
    fn run<'t>(&self, ctx: &mut AdminContext<'t>) -> Result<()> {
        let client = ctx.rest_client();
        let runtime = Runtime::new().unwrap();
        runtime.block_on(client.get_aptos_version()).unwrap();
        runtime.block_on(client.get_ledger_information()).unwrap();

        Ok(())
    }
}

pub async fn check_account_balance(
    client: &RestClient,
    account_address: AccountAddress,
    expected: u64,
) -> Result<()> {
    let balance = client
        .get_account_balance(account_address)
        .await?
        .into_inner();
    assert_eq!(balance.get(), expected);

    Ok(())
}

#[derive(Debug)]
struct FundAccount;

impl Test for FundAccount {
    fn name(&self) -> &'static str {
        "fund_account"
    }
}

#[async_trait::async_trait]
impl AptosTest for FundAccount {
    async fn run<'t>(&self, ctx: &mut AptosContext<'t>) -> Result<()> {
        let client = ctx.client();

        let account = ctx.random_account();
        let amount = 1000;
        ctx.create_user_account(account.public_key()).await?;
        ctx.mint(account.address(), amount).await?;
        check_account_balance(&client, account.address(), amount).await?;

        Ok(())
    }
}

#[derive(Debug)]
struct TransferCoins;

impl Test for TransferCoins {
    fn name(&self) -> &'static str {
        "transfer_coins"
    }
}

#[async_trait::async_trait]
impl AptosTest for TransferCoins {
    async fn run<'t>(&self, ctx: &mut AptosContext<'t>) -> Result<()> {
        let client = ctx.client();
        let mut payer = ctx.random_account();
        let payee = ctx.random_account();
        ctx.create_user_account(payer.public_key()).await?;
        ctx.create_user_account(payee.public_key()).await?;
        ctx.mint(payer.address(), 10000).await?;
        check_account_balance(&client, payer.address(), 10000).await?;

        let transfer_txn = payer.sign_with_transaction_builder(
            ctx.aptos_transaction_factory()
                .payload(aptos_stdlib::encode_test_coin_transfer(payee.address(), 10)),
        );
        client.submit_and_wait(&transfer_txn).await?;
        check_account_balance(&client, payee.address(), 10).await?;

        Ok(())
    }
}

#[derive(Debug)]
struct RestartValidator;

impl Test for RestartValidator {
    fn name(&self) -> &'static str {
        "restart_validator"
    }
}

impl NetworkTest for RestartValidator {
    fn run<'t>(&self, ctx: &mut NetworkContext<'t>) -> Result<()> {
        let runtime = Runtime::new()?;
        runtime.block_on(async {
            let node = ctx.swarm().validators_mut().next().unwrap();
            node.health_check().await.expect("node health check failed");
            node.stop().unwrap();
            println!("Restarting node {}", node.peer_id());
            node.start().await.unwrap();
            tokio::time::sleep(Duration::from_secs(1)).await;
            node.health_check().await.expect("node health check failed");
        });
        Ok(())
    }
}

#[derive(Debug)]
struct EmitTransaction;

impl Test for EmitTransaction {
    fn name(&self) -> &'static str {
        "emit_transaction"
    }
}

impl NetworkTest for EmitTransaction {
    fn run<'t>(&self, ctx: &mut NetworkContext<'t>) -> Result<()> {
        let duration = Duration::from_secs(10);
        let all_validators = ctx
            .swarm()
            .validators()
            .map(|v| v.peer_id())
            .collect::<Vec<_>>();
        let stats = generate_traffic(ctx, &all_validators, duration, 1, None).unwrap();
        ctx.report
            .report_txn_stats(self.name().to_string(), stats, duration);

        Ok(())
    }
}
