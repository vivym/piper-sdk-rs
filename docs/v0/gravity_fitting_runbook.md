# Gravity Fitting Hardware Validation Runbook

This runbook is for validating gravity fitting on real Piper hardware. Do not run
these commands blindly: a hardware operator must be present, support the arms
when needed, and confirm collision clearance before every motion-producing step.

## Preconditions

- Use a clear workspace with both arms physically supported during startup and
  first motion checks.
- Confirm the CAN interface is the intended hardware bus, for example `can0`.
- Keep the joint map and load profile consistent across data collection, model
  fitting, calibration, and teleoperation.
- Collect master and slave models separately when payloads differ, such as a
  master teaching gripper versus a follower normal gripper plus D405 payload.
- Prefer manual `record-path` followed by controlled `replay-sample`; this keeps
  sampling velocity and stability predictable.
- Start with reflection-only validation before enabling assist or combined mode.

## Data Collection and Fit Flow

Record the path manually:

```bash
cargo run -p piper-cli -- gravity record-path \
  --role slave \
  --interface can0 \
  --joint-map identity \
  --load-profile normal-gripper-d405 \
  --out artifacts/gravity/slave-d405.path.jsonl
```

Dry-run the replay before writing samples:

```bash
cargo run -p piper-cli -- gravity replay-sample \
  --role slave \
  --interface can0 \
  --path artifacts/gravity/slave-d405.path.jsonl \
  --out artifacts/gravity/slave-d405.samples.jsonl \
  --dry-run
```

Replay the path and collect samples:

```bash
cargo run -p piper-cli -- gravity replay-sample \
  --role slave \
  --interface can0 \
  --path artifacts/gravity/slave-d405.path.jsonl \
  --out artifacts/gravity/slave-d405.samples.jsonl
```

Fit the Rust model:

```bash
cargo run -p piper-cli -- gravity fit \
  --samples artifacts/gravity/slave-d405.samples.jsonl \
  --out artifacts/gravity/slave-d405.model.toml
```

Run the Python reference check:

```bash
uv run --project tools/gravity-reference \
  python tools/gravity-reference/gravity_fit_reference.py \
  --samples artifacts/gravity/slave-d405.samples.jsonl \
  --rust-model artifacts/gravity/slave-d405.model.toml \
  --out artifacts/gravity/slave-d405.reference-check.json
```

Evaluate the fitted model:

```bash
cargo run -p piper-cli -- gravity eval \
  --model artifacts/gravity/slave-d405.model.toml \
  --samples artifacts/gravity/slave-d405.samples.jsonl
```

Review the fit, reference-check, and eval outputs before using the model in
teleoperation. Do not proceed if the model has unexplained range violations,
unstable residuals, or payload/joint-map mismatches.

## Teleop Validation Order

Validate one mode at a time. Keep an operator at the arms and stop immediately
on any abnormal force, motion, or health report.

1. Reflection compensation only:

   ```text
   --gravity-reflection-compensation --slave-gravity-model ...
   ```

2. Master assist only, starting at a low ratio:

   ```text
   --master-gravity-model ... --master-gravity-assist-ratio 0.2
   ```

3. Slave assist only, starting at a low ratio:

   ```text
   --slave-gravity-model ... --slave-gravity-assist-ratio 0.2
   ```

4. Combined mode only after reflection-only, master-assist-only, and
   slave-assist-only validation have each passed separately.

For every step, keep the same model, joint map, load profile, and calibration
assumptions used during collection and fitting. If any payload changes, collect
and fit a new model for that arm before continuing.

## Safety Stop Criteria

Stop the run, disable teleop or motor output, and inspect logs before retrying
if any of these occur:

- The master arm pulls unexpectedly.
- Oscillation appears in either arm.
- Compensation confidence or range violations drop/repeat.
- Range violations occur in startup output or report summary.
- Raw-clock health failures occur.

After a stop, do not continue by increasing assist ratios or enabling combined
mode. Re-check the model file, role, payload, joint map, load profile, raw-clock
health, and collision clearance first.

## Validation Notes

- Use reflection-only mode to verify the slave model before any assist mode.
- Use low assist ratios first, such as `0.2`, and raise only after stable,
  repeatable behavior.
- Keep artifacts under a clear path such as `artifacts/gravity/` and avoid
  overwriting known-good path, sample, model, or reference-check files.
- Capture command output and operator observations for each stage so failures
  can be traced back to data collection, fitting, evaluation, or teleop setup.
