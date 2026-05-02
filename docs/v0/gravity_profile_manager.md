# Gravity Profile Manager Operator Guide

The gravity profile manager keeps path recordings, replayed torque samples,
fit reports, and the current best model together for one hardware identity.
Use one profile for exactly one arm, role, joint map, and load profile:

- `arm_id`: the physical arm identity, such as `piper-left`.
- `role`: the operating role, such as `master` or `slave`.
- `joint_map`: the joint ordering used during collection and teleop.
- `load_profile`: the payload identity used during collection and teleop.

Keep separate profiles when payloads differ. For example, a master arm with a
teaching handle and a follower arm with a gripper plus camera should not share
one gravity profile, even if their joint map is the same.

## Create A Profile

```bash
cargo run -p piper-cli -- gravity profile init \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --name slave-piper-left-normal-gripper-d405 \
  --role slave \
  --arm-id piper-left \
  --target socketcan:can1 \
  --joint-map identity \
  --load-profile normal-gripper-d405
```

`--profile` and `--name` are optional. If `--profile` is omitted, the CLI uses
`artifacts/gravity/profiles/<name>`. If `--name` is omitted, the default name is
`<role>-<arm_id>-<load_profile>`.

## Check Status

Use `status` for counts, identity, latest round, and current best model:

```bash
cargo run -p piper-cli -- gravity profile status \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405
```

Use `next` for the next operator action:

```bash
cargo run -p piper-cli -- gravity profile next \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405
```

Typical next actions are to collect train samples, collect validation samples,
run `fit-assess`, promote failed validation, or use the passing model.

## Collect Train Data

Record a collision-free manual path for training:

```bash
cargo run -p piper-cli -- gravity profile record-path \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split train \
  --notes "broad workspace sweep"
```

Dry-run the replay before moving the arm:

```bash
cargo run -p piper-cli -- gravity profile replay-sample \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split train \
  --path latest \
  --dry-run
```

Then replay the recorded path and collect training samples:

```bash
cargo run -p piper-cli -- gravity profile replay-sample \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split train \
  --path latest
```

`--path latest` selects the newest path artifact for that split. You can also
pass a registered path artifact id.

## Collect Validation Data

Validation should be fresh data that was not used for training. Record and
sample a separate validation path:

```bash
cargo run -p piper-cli -- gravity profile record-path \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split validation \
  --notes "held-out validation sweep"
```

```bash
cargo run -p piper-cli -- gravity profile replay-sample \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split validation \
  --path latest \
  --dry-run
```

```bash
cargo run -p piper-cli -- gravity profile replay-sample \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split validation \
  --path latest
```

## Replay Safety

`replay-sample` moves hardware unless `--dry-run` is set. Before every replay,
confirm the recorded path is collision-free for the current payload, fixtures,
and workspace. Keep an operator at the arm and stop on unexpected force,
oscillation, or contact.

Replay speed comes from `profile.toml` under `[replay]`. The defaults are
intentionally low: `max_velocity_rad_s = 0.08`, `max_step_rad = 0.02`,
`settle_ms = 500`, `sample_ms = 300`, and `bidirectional = true`.

## Import Offline Samples

For multi-session collection or offline workflows, import existing sample
artifacts into the profile instead of replaying immediately:

```bash
cargo run -p piper-cli -- gravity profile import-samples \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split train \
  --samples session-a.samples.jsonl \
  --samples session-b.samples.jsonl
```

```bash
cargo run -p piper-cli -- gravity profile import-samples \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405 \
  --split validation \
  --samples session-c.validation.samples.jsonl
```

Imported samples must match the profile identity fields. Legacy samples without
an `arm_id` are accepted as profile-asserted data and tracked in the manifest.

## Fit And Assess

Run fitting and validation after both train and validation sample artifacts are
present:

```bash
cargo run -p piper-cli -- gravity profile fit-assess \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405
```

`fit-assess` writes a round model, report, and provenance record. If the strict
gate passes, it also promotes `models/best.model.toml` as the usable model for
that profile. Use `status` after fitting to see whether the profile passed,
needs more data, failed fitting, or failed validation.

## Validation Failure Loop

When `status` reports `validation_failed`, treat the validation set as useful
coverage that the training set missed. Promote that validation data into train:

```bash
cargo run -p piper-cli -- gravity profile promote-validation \
  --profile artifacts/gravity/profiles/slave-piper-left-normal-gripper-d405
```

Then collect fresh validation data and fit again:

```text
train data exists
collect validation -> fit-assess
if validation_failed:
  promote-validation
  collect fresh validation
  fit-assess again
repeat until passed
```

Do not reuse the promoted validation data as the next validation set. The next
fit needs newly collected or newly imported validation samples.

## Output Layout

Each profile directory is self-contained:

- `profile.toml`: profile identity, target, replay settings, fit settings, and
  validation gates.
- `manifest.json`: registered artifacts, status, events, rounds, hashes, and the
  current best model pointer.
- `data/train/paths` and `data/validation/paths`: recorded path JSONL files.
- `data/train/samples` and `data/validation/samples`: replayed or imported
  sample JSONL files.
- `models/<round>.model.toml`: per-round fitted models.
- `models/best.model.toml`: latest model that passed validation gates.
- `reports/<round>.assess.json`: fit and validation assessment reports.
- `rounds/<round>.json`: round provenance used to reproduce the decision.
