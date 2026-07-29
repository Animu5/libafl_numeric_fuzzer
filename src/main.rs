mod measurement;
mod openocd;

use clap::Parser;
use libafl::mutators::{havoc_mutations, HavocScheduledMutator};
use libafl::{
    corpus::{InMemoryCorpus, OnDiskCorpus},
    events::SimpleEventManager,
    executors::{ExitKind, InProcessExecutor},
    feedbacks::{Feedback, StateInitializer},
    fuzzer::{Fuzzer, StdFuzzer},
    inputs::{BytesInput, HasTargetBytes},
    monitors::NopMonitor,
    schedulers::QueueScheduler,
    stages::mutational::StdMutationalStage,
    state::StdState,
    Error,
};
use libafl_bolts::{current_nanos, rands::StdRand, tuples::tuple_list, AsSlice, Named};
use measurement::{FileMeasurement, Measurement};
use std::{borrow::Cow, fs::write, path::PathBuf, thread, time::Duration};
use openocd::OpenOcd;

#[derive(Parser, Debug)]
#[command(version, long_version = None, about, long_about = None)]
struct Args{
    /// Path to executable to fuzz
    #[arg(short, long)]
    executable: PathBuf,

    /// Path to assembly environment
    #[arg(short, long)]
    assembly_environment: PathBuf,

    /// Path to atndecode
    #[arg(short, long)]
    atndecode_path: PathBuf,

    /// Directory of best snippets
    #[arg(short, long)]
    solutions_path: PathBuf,
}

#[derive(Clone, Debug)]
struct NumericFeedback<M: Measurement> {
    best: f64,
    measurement: M,
    name: Cow<'static, str>,
}

impl<M: Measurement> NumericFeedback<M> {
    fn new(measurement: M) -> Self {
        Self {
            best: f64::MIN,
            measurement,
            name: Cow::Borrowed("NumericFeedback"),
        }
    }
}

impl<M: Measurement> Named for NumericFeedback<M> {
    fn name(&self) -> &Cow<'static, str> {
        &self.name
    }
}

impl<S, M: Measurement> StateInitializer<S> for NumericFeedback<M> {}

impl<EM, I, OT, S, M: Measurement> Feedback<EM, I, OT, S> for NumericFeedback<M> {
    fn is_interesting(
        &mut self,
        _state: &mut S,
        _manager: &mut EM,
        _input: &I,
        _observers: &OT,
        _exit_kind: &ExitKind,
    ) -> Result<bool, Error> {
        let value = self.measurement.measure();

        if value > self.best {
            self.best = value;
            println!("New best measurement: {}", value);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn last_result(&self) -> Result<bool, Error> {
        Ok(false)
    }
}

fn main() -> Result<(), Error> {
    let args = Args::parse();

    if args.solutions_path.exists() && !args.solutions_path.is_dir() {
        panic!("Solutions path exists but is not a directory");
    }
    std::fs::create_dir_all(&args.solutions_path).expect("Failed to create solutions directory.");

    let binary_path = args.executable;

    let program_start_addr: u32 = 0x80000000;

    // Connect to OpenOCD only once.
    let mut openocd = OpenOcd::new("127.0.0.1:6666").expect("Could not connect to OpenOCD.");

    // Load ELF program using OpenOCD `load`.
    openocd
        .load_program(&binary_path, program_start_addr)
        .expect("Failed to load program.");

    // Measure idle power before running.
    let idle_measurement = FileMeasurement::new("power.txt");
    let idle_power = idle_measurement.measure();
    println!("Idle power: {}", idle_power);

    // Resume once to get running measurement.
    openocd.resume().expect("Failed to resume target");

    thread::sleep(Duration::from_millis(100));

    let running_measurement = FileMeasurement::new("power.txt");
    let running_power = running_measurement.measure();
    println!("Running power: {}", running_power);

    let rand = StdRand::with_seed(current_nanos());

    let corpus = InMemoryCorpus::<BytesInput>::new();
    let solutions = OnDiskCorpus::new(args.solutions_path)?;

    let measurement = FileMeasurement::new("power.txt");
    let mut feedback = NumericFeedback::new(measurement);

    let mut objective = ();

    let mut state = StdState::new(rand, corpus, solutions, &mut feedback, &mut objective)?;

    let scheduler = QueueScheduler::new();

    let mut fuzzer = StdFuzzer::new(scheduler, feedback, objective);

    let monitor = NopMonitor::new();
    let mut manager = SimpleEventManager::new(monitor);

    let mut target = |input: &BytesInput| -> ExitKind {
        let bytes = input.target_bytes();
        let data = bytes.as_slice();

        // Save current fuzz input locally for debugging.
        if let Err(e) = write("current_input.bin", data) {
            eprintln!("Failed to write current_input.bin: {}", e);
            return ExitKind::Crash;
        }

        if let Err(e) = openocd.reset_init() {
            eprintln!("OpenOCD reset init failed: {}", e);
            return ExitKind::Crash;
        }

        // Reset program counter to program start.
        if let Err(e) = openocd.set_pc(program_start_addr) {
            eprintln!("Setting PC failed: {}", e);
            return ExitKind::Crash;
        }

        // Resume target execution.
        if let Err(e) = openocd.resume() {
            eprintln!("OpenOCD resume failed: {}", e);
            return ExitKind::Crash;
        }

        // Give the target some time to execute before measurement happens.
        thread::sleep(Duration::from_millis(100));

        ExitKind::Ok
    };

    let mut executor = InProcessExecutor::new(
        &mut target,
        tuple_list!(),
        &mut fuzzer,
        &mut state,
        &mut manager,
    )?;

    let mutator = HavocScheduledMutator::new(havoc_mutations());
    let stage = StdMutationalStage::new(mutator);
    let mut stages = tuple_list!(stage);

    fuzzer.fuzz_loop(&mut stages, &mut executor, &mut state, &mut manager)?;

    Ok(())
}
