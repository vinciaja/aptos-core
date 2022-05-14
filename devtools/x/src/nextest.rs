// Copyright (c) Aptos
// SPDX-License-Identifier: Apache-2.0

use crate::{
    cargo::{build_args::BuildArgs, selected_package::SelectedPackageArgs, CargoCommand},
    context::XContext,
    Result,
};
use anyhow::{bail, Context};
use camino::Utf8PathBuf;
use nextest_runner::{
    config::NextestConfig,
    partition::PartitionerBuilder,
    reporter::{StatusLevel, TestOutputDisplay, TestReporterBuilder},
    runner::TestRunnerBuilder,
    signal::SignalHandler,
    target_runner::TargetRunner,
    test_filter::{RunIgnored, TestFilterBuilder},
    test_list::{BinaryList, RustTestArtifact, TestList},
};
use std::{ffi::OsString, io::Cursor};
use structopt::StructOpt;
use supports_color::Stream;

#[derive(Debug, StructOpt)]
pub struct Args {
    /// Nextest profile to use
    #[structopt(long, short = "P")]
    nextest_profile: Option<String>,
    /// Config file [default: workspace-root/.config/nextest.toml]
    config_file: Option<Utf8PathBuf>,
    #[structopt(flatten)]
    pub(crate) package_args: SelectedPackageArgs,
    #[structopt(long, short)]
    /// Skip running expensive testsuite integration tests
    unit: bool,
    #[structopt(flatten)]
    pub(crate) build_args: BuildArgs,
    #[structopt(flatten)]
    pub(crate) runner_opts: TestRunnerOpts,
    #[structopt(flatten)]
    reporter_opts: TestReporterOpts,
    #[structopt(long)]
    /// Do not run tests, only compile the test executables
    no_run: bool,
    /// Run ignored tests
    #[structopt(long, possible_values = &RunIgnored::variants(), default_value, case_insensitive = true)]
    run_ignored: RunIgnored,
    /// Test partition, e.g. hash:1/2 or count:2/3
    #[structopt(long)]
    partition: Option<PartitionerBuilder>,
    #[structopt(name = "FILTERS", last = true)]
    filters: Vec<String>,
}

/// Test runner options.
#[derive(Debug, Default, StructOpt)]
pub struct TestRunnerOpts {
    /// Number of retries for failing tests [default: from profile]
    #[structopt(long)]
    retries: Option<usize>,

    /// Cancel test run on the first failure
    #[structopt(long)]
    fail_fast: bool,

    /// Run all tests regardless of failure
    #[structopt(long, overrides_with = "fail-fast")]
    no_fail_fast: bool,

    /// Number of tests to run simultaneously [default: logical CPU count]
    #[structopt(long)]
    test_threads: Option<usize>,
}

impl TestRunnerOpts {
    fn to_builder(&self) -> TestRunnerBuilder {
        let mut builder = TestRunnerBuilder::default();
        if let Some(retries) = self.retries {
            builder.set_retries(retries);
        }
        if self.no_fail_fast {
            builder.set_fail_fast(false);
        } else if self.fail_fast {
            builder.set_fail_fast(true);
        }
        if let Some(test_threads) = self.test_threads {
            builder.set_test_threads(test_threads);
        }

        builder
    }
}

#[derive(Debug, Default, StructOpt)]
#[structopt(rename_all = "kebab-case")]
pub struct TestReporterOpts {
    /// Output stdout and stderr on failure
    #[structopt(long, possible_values = TestOutputDisplay::variants(), case_insensitive = true)]
    failure_output: Option<TestOutputDisplay>,
    /// Output stdout and stderr on success
    #[structopt(long, possible_values = TestOutputDisplay::variants(), case_insensitive = true)]
    success_output: Option<TestOutputDisplay>,
    /// Test statuses to output
    #[structopt(long, possible_values = StatusLevel::variants(), case_insensitive = true)]
    status_level: Option<StatusLevel>,
}

impl TestReporterOpts {
    fn to_builder(&self) -> TestReporterBuilder {
        let mut builder = TestReporterBuilder::default();
        if let Some(failure_output) = self.failure_output {
            builder.set_failure_output(failure_output);
        }
        if let Some(success_output) = self.success_output {
            builder.set_success_output(success_output);
        }
        if let Some(status_level) = self.status_level {
            builder.set_status_level(status_level);
        }
        builder
    }
}

pub fn run(args: Args, xctx: XContext) -> Result<()> {
    let config = xctx.config();

    let mut packages = args.package_args.to_selected_packages(&xctx)?;
    if args.unit {
        packages.add_excludes(config.system_tests().iter().map(|(p, _)| p.as_str()));
    }

    let mut direct_args = Vec::new();
    args.build_args.add_args(&mut direct_args);

    // Always pass in --no-run as the test runner is responsible for running these tests.
    direct_args.push(OsString::from("--no-run"));

    // TODO: no-fail-fast (needs support in nextest)

    // Step 1: build all the test binaries with --no-run.
    let cmd = CargoCommand::Test {
        cargo_config: xctx.config().cargo_config(),
        direct_args: direct_args.as_slice(),
        // Don't pass in the args (test name) -- they're for use by the test runner.
        args: &[],
        env: &[],
        skip_sccache: false,
    };

    let stdout = cmd.run_capture_stdout(&packages)?;

    if args.no_run {
        // Don't proceed further.
        return Ok(());
    }

    let package_graph = xctx.core().package_graph()?;
    let workspace = package_graph.workspace();

    let config = NextestConfig::from_sources(workspace.root(), args.config_file.as_deref())?;
    let profile = config.profile(
        args.nextest_profile
            .as_deref()
            .unwrap_or(NextestConfig::DEFAULT_PROFILE),
    )?;

    let test_binaries = BinaryList::from_messages(Cursor::new(stdout), package_graph)?;

    let test_filter = TestFilterBuilder::new(args.run_ignored, args.partition, &args.filters);
    let test_artifacts =
        RustTestArtifact::from_binary_list(package_graph, test_binaries, None, None)?;
    let test_list = TestList::new(test_artifacts, &test_filter, &TargetRunner::empty())?;

    let handler = SignalHandler::new().context("failed to install nextest signal handler")?;
    let runner =
        args.runner_opts
            .to_builder()
            .build(&test_list, &profile, handler, TargetRunner::empty());

    let mut reporter = args.reporter_opts.to_builder().build(&test_list, &profile);
    if args.build_args.color.should_colorize(Stream::Stderr) {
        reporter.colorize();
    }

    let stderr = std::io::stderr();
    let run_stats = runner.try_execute(|event| reporter.report_event(event, stderr.lock()))?;
    if !run_stats.is_success() {
        bail!("test run failed");
    }

    Ok(())
}
