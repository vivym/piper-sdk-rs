# Piper SVS Collector

`piper-svs-collect` is the SVSPolicy data-collection teleoperation binary. It is an addon crate, intentionally outside the default workspace so the normal SDK, `piper-cli`, and `piper-control` remain MuJoCo-free.

## Runtime Requirements

- Linux with SocketCAN targets only. The collector is intended for StrictRealtime SocketCAN operation.
- Two Piper arms on separate CAN interfaces, for example `socketcan:can0` and `socketcan:can1`.
- MuJoCo 3.3.7 native libraries. In this repository, use `just _mujoco_download addons/piper-physics-mujoco/Cargo.toml` or the `just build-physics` / `just test-physics` helpers.
- A resolved calibration file, or a safe posture where the collector can capture and persist one before MIT mode is enabled.

## Startup Order

The collector validates static CLI inputs, resolves the task profile, validates and hashes the MuJoCo model, resolves the configured end-effector sites, connects both arms, resolves calibration, reserves an episode directory, writes `effective_profile.toml`, writes canonical `calibration.toml`, writes a running manifest, starts the writer, optionally starts raw CAN side recording, asks for confirmation unless `--yes` is set, then enables MIT mode and enters the bilateral loop.

Any failure before MIT enable finalizes the episode without enabling MIT. Operator cancellation or declined confirmation finalizes as `cancelled` only if the writer is clean. Writer queue-full, dropped steps, writer flush failure, tick mismatch, or telemetry-sink failure finalizes as `faulted`.

## Episode Layout

Each run creates:

- `manifest.toml`: reproducibility metadata, target resolution, MuJoCo identity, calibration hash, raw CAN status, and step summary.
- `report.json`: run outcome, writer stats, dual-arm loop stats, raw CAN degradation status, and final flush result.
- `effective_profile.toml`: canonical resolved profile bytes used for this run.
- `calibration.toml`: canonical calibration bytes used for this run.
- `steps.bin`: binary `SvsEpisodeV1` dataset records.
- `raw_can/master.piperrec` and `raw_can/slave.piperrec` when `--raw-can` is enabled and raw capture starts successfully.

Episode directories are reserved under `<output-dir>/<task-slug>/<episode-id>` and are never overwritten.

## Example Commands

```bash
eval "$(just _mujoco_download addons/piper-physics-mujoco/Cargo.toml)"

cargo run --manifest-path addons/piper-svs-collect/Cargo.toml -- \
  --master-target socketcan:can0 \
  --slave-target socketcan:can1 \
  --model-dir /path/to/mujoco/model-dir \
  --task-profile addons/piper-svs-collect/profiles/surface_following.toml \
  --output-dir /data/svs \
  --calibration-file /data/calibration.toml \
  --task "surface following" \
  --operator viv \
  --raw-can \
  --yes
```

For HIL acceptance, start with a short run:

```bash
eval "$(just _mujoco_download addons/piper-physics-mujoco/Cargo.toml)"
cargo run --manifest-path addons/piper-svs-collect/Cargo.toml -- \
  --master-target socketcan:can0 \
  --slave-target socketcan:can1 \
  --use-embedded-model \
  --task-profile addons/piper-svs-collect/profiles/surface_following.toml \
  --output-dir /tmp/svs-acceptance \
  --max-iterations 20 \
  --yes
```

## Experimental Calibrated Raw-Clock SVS

Use this only after the general teleop raw-clock smoke script is stable on the same interfaces.

```bash
cargo run --manifest-path addons/piper-svs-collect/Cargo.toml -- \
  --master-target socketcan:can1 \
  --slave-target socketcan:can0 \
  --use-embedded-model \
  --output-dir artifacts/svs \
  --mirror-map identity \
  --disable-gripper-mirror \
  --experimental-calibrated-raw \
  --raw-clock-warmup-secs 10 \
  --raw-clock-residual-p95-us 2000 \
  --raw-clock-residual-max-us 3000 \
  --raw-clock-residual-max-consecutive-failures 3 \
  --raw-clock-sample-gap-max-ms 50 \
  --raw-clock-inter-arm-skew-max-us 20000 \
  --raw-clock-state-skew-max-us 10000 \
  --raw-clock-alignment-lag-us 5000 \
  --raw-clock-alignment-search-window-us 25000
```

The raw-clock backend is explicit and never falls back to StrictRealtime. The first implementation requires `--disable-gripper-mirror`.

## Starter Profiles

The files in `profiles/` are conservative starting points. They use explicit placeholder MuJoCo site names:

- `master_tool_center`
- `slave_tool_center`

Change those names if the selected model uses different end-effector sites. Tune gains on hardware gradually; do not treat these profiles as task-optimal parameters.

## Verification

```bash
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --test dependency_boundaries -- --nocapture

eval "$(just _mujoco_download addons/piper-physics-mujoco/Cargo.toml)"
cargo fmt --manifest-path addons/piper-physics-mujoco/Cargo.toml -- --check
cargo test --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets
cargo clippy --manifest-path addons/piper-physics-mujoco/Cargo.toml --all-targets --all-features -- -D warnings
cargo fmt --manifest-path addons/piper-svs-collect/Cargo.toml -- --check
cargo test --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets
cargo clippy --manifest-path addons/piper-svs-collect/Cargo.toml --all-targets --all-features -- -D warnings
```
