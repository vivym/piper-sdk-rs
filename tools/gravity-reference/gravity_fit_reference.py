from __future__ import annotations

import argparse
import json
import math
import sys
import tomllib
from pathlib import Path
from typing import Any

import numpy as np


JOINT_COUNT = 6
TRIG_V1_FEATURE_COUNT = 23
SAMPLE_ARTIFACT_KIND = "quasi-static-samples"
SAMPLE_ROW_TYPE = "quasi-static-sample"
HEADER_ROW_TYPE = "header"
TORQUE_CONVENTION = "piper-sdk-normalized-nm-v1"
MODEL_KIND = "joint-space-quasi-static-torque"
BASIS_TRIG_V1 = "trig-v1"
HEADER_KEYS = {
    "type",
    "artifact_kind",
    "schema_version",
    "source_path",
    "source_sha256",
    "role",
    "target",
    "joint_map",
    "load_profile",
    "torque_convention",
    "frequency_hz",
    "max_velocity_rad_s",
    "max_step_rad",
    "settle_ms",
    "sample_ms",
    "stable_velocity_rad_s",
    "stable_tracking_error_rad",
    "stable_torque_std_nm",
    "waypoint_count",
    "accepted_waypoint_count",
    "rejected_waypoint_count",
}
REQUIRED_SAMPLE_ROW_KEYS = {
    "type",
    "waypoint_id",
    "pass_direction",
    "host_mono_us",
    "q_rad",
    "dq_rad_s",
    "tau_nm",
    "position_valid_mask",
    "dynamic_valid_mask",
    "stable_velocity_rad_s",
    "stable_tracking_error_rad",
    "stable_torque_std_nm",
}
OPTIONAL_SAMPLE_ROW_KEYS = {"raw_timestamp_us", "segment_id"}


def trig_v1_feature_names() -> list[str]:
    names = ["bias"]
    for joint in range(1, JOINT_COUNT + 1):
        names.append(f"sin_q{joint}")
        names.append(f"cos_q{joint}")
    for joint in range(1, JOINT_COUNT):
        names.append(f"sin_q{joint}_plus_q{joint + 1}")
        names.append(f"cos_q{joint}_plus_q{joint + 1}")
    return names


def trig_v1_features(q: np.ndarray) -> np.ndarray:
    q = np.asarray(q, dtype=np.float64)
    if q.shape != (JOINT_COUNT,):
        raise ValueError(f"expected q shape ({JOINT_COUNT},), got {q.shape}")
    if not np.all(np.isfinite(q)):
        raise ValueError("q must contain only finite values")

    features = np.empty(TRIG_V1_FEATURE_COUNT, dtype=np.float64)
    features[0] = 1.0
    out = 1
    for value in q:
        features[out] = math.sin(float(value))
        features[out + 1] = math.cos(float(value))
        out += 2
    for joint in range(JOINT_COUNT - 1):
        value = float(q[joint] + q[joint + 1])
        features[out] = math.sin(value)
        features[out + 1] = math.cos(value)
        out += 2
    return features


def load_samples(paths: list[Path]) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    if not paths:
        raise ValueError("expected at least one quasi-static-samples artifact path")

    loaded_header: dict[str, Any] | None = None
    rows: list[dict[str, Any]] = []
    for path in paths:
        header, file_rows = _load_samples_file(Path(path), loaded_header)
        if loaded_header is None:
            loaded_header = header
        rows.extend(file_rows)

    assert loaded_header is not None
    return loaded_header, rows


def make_holdout_groups(
    rows: list[dict[str, Any]], holdout_ratio: float
) -> tuple[list[str], list[str]]:
    if not math.isfinite(holdout_ratio) or holdout_ratio < 0.0 or holdout_ratio >= 1.0:
        raise ValueError("holdout_ratio must be finite and in [0.0, 1.0)")

    all_groups = sorted({_group_id_for_row(row) for row in rows})
    holdout_groups: set[str] = set()
    if holdout_ratio > 0.0:
        if len(all_groups) < 2:
            raise ValueError("holdout requires at least 2 distinct groups")
        stride = max(1, int(math.floor((1.0 / holdout_ratio) + 0.5)))
        for index, group_id in enumerate(all_groups):
            if index % stride == 0:
                holdout_groups.add(group_id)
        if not holdout_groups and all_groups:
            holdout_groups.add(all_groups[0])

    train_group_ids = [group_id for group_id in all_groups if group_id not in holdout_groups]
    if not train_group_ids:
        raise ValueError("holdout split left no training groups; choose a smaller holdout_ratio")
    return train_group_ids, sorted(holdout_groups)


def solve_ridge(
    x: np.ndarray,
    y: np.ndarray,
    ridge_lambda: float,
    regularize_bias: bool,
) -> np.ndarray:
    x = np.asarray(x, dtype=np.float64)
    y = np.asarray(y, dtype=np.float64)
    if x.ndim != 2 or x.shape[1] != TRIG_V1_FEATURE_COUNT:
        raise ValueError(f"expected x shape (n, {TRIG_V1_FEATURE_COUNT}), got {x.shape}")
    if y.ndim == 1:
        if y.shape[0] != x.shape[0]:
            raise ValueError("expected y row count to match x")
    elif y.ndim == 2:
        if y.shape[0] != x.shape[0]:
            raise ValueError("expected y row count to match x")
    else:
        raise ValueError("expected y to be a vector or matrix")
    if not math.isfinite(ridge_lambda) or ridge_lambda <= 0.0:
        raise ValueError("ridge_lambda must be finite and > 0.0")
    if not np.all(np.isfinite(x)) or not np.all(np.isfinite(y)):
        raise ValueError("x and y must contain only finite values")

    g = x.T @ x
    b = x.T @ y
    ridge_diag = np.full(TRIG_V1_FEATURE_COUNT, ridge_lambda, dtype=np.float64)
    if not regularize_bias:
        ridge_diag[0] = 0.0
    g = g + np.diag(ridge_diag)

    try:
        return np.linalg.solve(g, b)
    except np.linalg.LinAlgError:
        solution, *_ = np.linalg.lstsq(g, b, rcond=1e-12)
        return solution


def load_rust_model(path: Path) -> dict[str, Any]:
    with Path(path).open("rb") as f:
        return tomllib.load(f)


def compare_model(
    samples: tuple[dict[str, Any], list[dict[str, Any]]],
    rust_model: dict[str, Any],
    tolerances: dict[str, float],
) -> dict[str, Any]:
    _validate_rust_model(rust_model)
    _header, rows = samples
    if not rows:
        raise ValueError("expected at least one quasi-static sample row")

    (
        ridge_lambda,
        regularize_bias,
        holdout_ratio,
        rust_train_group_ids,
        rust_holdout_group_ids,
    ) = _validate_fit_metadata(rust_model)

    train_group_ids, holdout_group_ids = make_holdout_groups(rows, holdout_ratio)
    holdout_group_set = set(holdout_group_ids)
    train_rows = [row for row in rows if _group_id_for_row(row) not in holdout_group_set]
    holdout_rows = [row for row in rows if _group_id_for_row(row) in holdout_group_set]
    if len(train_group_ids) < 2:
        raise ValueError("expected at least 2 training groups")
    if len(train_rows) < TRIG_V1_FEATURE_COUNT:
        raise ValueError(
            f"expected at least {TRIG_V1_FEATURE_COUNT} training samples, got {len(train_rows)}"
        )

    x_train = np.stack([trig_v1_features(np.array(row["q_rad"])) for row in train_rows])
    y_train = np.stack([_joint_array(row, "tau_nm") for row in train_rows])
    python_coefficients = solve_ridge(
        x_train,
        y_train,
        ridge_lambda=ridge_lambda,
        regularize_bias=regularize_bias,
    )

    rust_coefficients = np.asarray(rust_model["model"]["coefficients_nm"], dtype=np.float64).T
    coefficient_max_abs_diff = float(
        np.max(np.abs(python_coefficients - rust_coefficients))
    )

    train_metrics = _residual_metrics(train_rows, python_coefficients)
    holdout_metrics = _residual_metrics(holdout_rows, python_coefficients)
    residual_metric_max_abs_diff = _residual_metric_max_abs_diff(
        train_metrics, holdout_metrics, rust_model.get("fit_quality", {})
    )

    holdout_split_matches = (
        train_group_ids == rust_train_group_ids
        and holdout_group_ids == rust_holdout_group_ids
    )
    pass_ = (
        coefficient_max_abs_diff <= float(tolerances["coefficient_atol"])
        and residual_metric_max_abs_diff <= float(tolerances["residual_atol"])
        and holdout_split_matches
    )

    return {
        "pass": pass_,
        "coefficient_max_abs_diff": coefficient_max_abs_diff,
        "residual_metric_max_abs_diff": residual_metric_max_abs_diff,
        "holdout_split_matches": holdout_split_matches,
    }


def write_synthetic_samples(path: Path, sample_count: int = 600) -> None:
    path = Path(path)
    if path.parent and str(path.parent) != ".":
        path.parent.mkdir(parents=True, exist_ok=True)

    header = {
        "type": HEADER_ROW_TYPE,
        "artifact_kind": SAMPLE_ARTIFACT_KIND,
        "schema_version": 1,
        "source_path": "synthetic",
        "source_sha256": "synthetic",
        "role": "slave",
        "target": "synthetic",
        "joint_map": "piper_default",
        "load_profile": "unloaded",
        "torque_convention": TORQUE_CONVENTION,
        "frequency_hz": 100.0,
        "max_velocity_rad_s": 0.08,
        "max_step_rad": 0.02,
        "settle_ms": 500,
        "sample_ms": 300,
        "stable_velocity_rad_s": 0.01,
        "stable_tracking_error_rad": 0.03,
        "stable_torque_std_nm": 0.08,
        "waypoint_count": sample_count,
        "accepted_waypoint_count": sample_count,
        "rejected_waypoint_count": 0,
    }
    coefficients = _synthetic_truth_coefficients()

    with path.open("w", encoding="utf-8") as f:
        f.write(json.dumps(header, sort_keys=True, separators=(",", ":")) + "\n")
        for sample_index in range(sample_count):
            q_rad = _synthetic_q(sample_index)
            tau_nm = trig_v1_features(q_rad) @ coefficients
            row = {
                "type": SAMPLE_ROW_TYPE,
                "waypoint_id": sample_index,
                "segment_id": f"segment-{sample_index // 20}",
                "pass_direction": "forward" if sample_index % 2 == 0 else "backward",
                "host_mono_us": sample_index * 10_000,
                "raw_timestamp_us": None,
                "q_rad": _float_list(q_rad),
                "dq_rad_s": [0.0] * JOINT_COUNT,
                "tau_nm": _float_list(tau_nm),
                "position_valid_mask": 0x3F,
                "dynamic_valid_mask": 0x3F,
                "stable_velocity_rad_s": 0.0,
                "stable_tracking_error_rad": 0.0,
                "stable_torque_std_nm": 0.0,
            }
            f.write(json.dumps(row, sort_keys=True, separators=(",", ":")) + "\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Reference gravity fit checker")
    parser.add_argument("--samples", action="append", type=Path, default=[])
    parser.add_argument("--rust-model", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--write-synthetic-samples", type=Path)
    parser.add_argument("--coefficient-atol", type=float, default=1e-7)
    parser.add_argument("--residual-atol", type=float, default=1e-7)
    args = parser.parse_args(argv)

    if args.write_synthetic_samples is not None:
        write_synthetic_samples(args.write_synthetic_samples)
        if not args.samples and args.rust_model is None:
            report = {
                "pass": True,
                "coefficient_max_abs_diff": 0.0,
                "residual_metric_max_abs_diff": 0.0,
                "holdout_split_matches": True,
            }
            _write_report(report, args.out)
            return 0

    if not args.samples:
        parser.error("--samples is required unless only --write-synthetic-samples is used")
    if args.rust_model is None:
        parser.error("--rust-model is required unless only --write-synthetic-samples is used")

    samples = load_samples(args.samples)
    rust_model = load_rust_model(args.rust_model)
    report = compare_model(
        samples,
        rust_model,
        {
            "coefficient_atol": args.coefficient_atol,
            "residual_atol": args.residual_atol,
        },
    )
    _write_report(report, args.out)
    return 0 if report["pass"] else 1


def _load_samples_file(
    path: Path, loaded_header: dict[str, Any] | None
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    with path.open("r", encoding="utf-8") as f:
        header_line = f.readline()
        if not header_line:
            raise ValueError(f"{path} is empty; expected quasi-static-samples header")

        header = json.loads(header_line)
        if header.get("type") != HEADER_ROW_TYPE:
            raise ValueError(
                f"{path} first row type must be {HEADER_ROW_TYPE!r}, got {header.get('type')!r}"
            )
        if header.get("artifact_kind") != SAMPLE_ARTIFACT_KIND:
            raise ValueError(
                f"{path} artifact_kind must be {SAMPLE_ARTIFACT_KIND!r}, "
                f"got {header.get('artifact_kind')!r}"
            )
        _validate_header(header, path)
        _validate_header_matches(loaded_header, header, path)

        rows = []
        for line_number, line in enumerate(f, start=2):
            row = json.loads(line)
            _validate_sample_row(row, path, line_number)
            rows.append(row)
    return header, rows


def _validate_header(header: dict[str, Any], path: Path) -> None:
    _validate_keys(header, HEADER_KEYS, set(), f"{path} header")
    if _require_nonnegative_int(header.get("schema_version"), f"{path} schema_version") != 1:
        raise ValueError(f"{path} schema_version must be 1")
    if header.get("artifact_kind") != SAMPLE_ARTIFACT_KIND:
        raise ValueError(
            f"{path} artifact_kind must be {SAMPLE_ARTIFACT_KIND!r}, "
            f"got {header.get('artifact_kind')!r}"
        )
    if header.get("torque_convention") != TORQUE_CONVENTION:
        raise ValueError(
            f"{path} torque_convention must be {TORQUE_CONVENTION!r}, "
            f"got {header.get('torque_convention')!r}"
        )
    for key in (
        "frequency_hz",
        "max_velocity_rad_s",
        "max_step_rad",
        "stable_velocity_rad_s",
        "stable_tracking_error_rad",
        "stable_torque_std_nm",
    ):
        _require_finite_number(header.get(key), f"{path} {key}")
    for key in (
        "settle_ms",
        "sample_ms",
        "waypoint_count",
        "accepted_waypoint_count",
        "rejected_waypoint_count",
    ):
        _require_nonnegative_int(header.get(key), f"{path} {key}")


def _validate_header_matches(
    loaded_header: dict[str, Any] | None, header: dict[str, Any], path: Path
) -> None:
    if loaded_header is None:
        return
    for key in ("role", "joint_map", "load_profile", "torque_convention"):
        if loaded_header.get(key) != header.get(key):
            raise ValueError(
                f"{path} {key} {header.get(key)!r} does not match first artifact "
                f"{key} {loaded_header.get(key)!r}"
            )


def _validate_sample_row(row: dict[str, Any], path: Path, line_number: int) -> None:
    _validate_keys(
        row,
        REQUIRED_SAMPLE_ROW_KEYS,
        OPTIONAL_SAMPLE_ROW_KEYS,
        f"{path} row {line_number}",
    )
    if row.get("type") != SAMPLE_ROW_TYPE:
        raise ValueError(
            f"{path} row {line_number} type must be {SAMPLE_ROW_TYPE!r}, "
            f"got {row.get('type')!r}"
        )
    if row.get("pass_direction") not in {"forward", "backward"}:
        raise ValueError(f"{path} row {line_number} pass_direction is invalid")
    for key in ("waypoint_id", "host_mono_us"):
        _require_nonnegative_int(row.get(key), f"{path} row {line_number} {key}")
    for key in ("position_valid_mask", "dynamic_valid_mask"):
        _require_mask_u8(row.get(key), f"{path} row {line_number} {key}")
    raw_timestamp_us = row.get("raw_timestamp_us")
    if raw_timestamp_us is not None:
        _require_nonnegative_int(
            raw_timestamp_us, f"{path} row {line_number} raw_timestamp_us"
        )
    for key in (
        "stable_velocity_rad_s",
        "stable_tracking_error_rad",
        "stable_torque_std_nm",
    ):
        _require_finite_number(row.get(key), f"{path} row {line_number} {key}")
    for key in ("q_rad", "dq_rad_s", "tau_nm"):
        _joint_array(row, key, label=f"{path} row {line_number} {key}")


def _validate_keys(
    value: dict[str, Any], required: set[str], optional: set[str], label: str
) -> None:
    keys = set(value)
    missing = sorted(required - keys)
    if missing:
        raise ValueError(f"{label} missing required field(s): {', '.join(missing)}")
    extra = sorted(keys - required - optional)
    if extra:
        raise ValueError(f"{label} has unknown field(s): {', '.join(extra)}")


def _require_finite_number(value: Any, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{label} must be finite")
    value = float(value)
    if not math.isfinite(value):
        raise ValueError(f"{label} must be finite")
    return value


def _require_nonnegative_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise ValueError(f"{label} must be a nonnegative integer")
    return value


def _require_mask_u8(value: Any, label: str) -> int:
    value = _require_nonnegative_int(value, label)
    if value > 255:
        raise ValueError(f"{label} must be in 0..255")
    return value


def _joint_array(row: dict[str, Any], key: str, label: str | None = None) -> np.ndarray:
    value = np.asarray(row.get(key), dtype=np.float64)
    if value.shape != (JOINT_COUNT,):
        raise ValueError(f"{label or key} must have {JOINT_COUNT} values")
    if not np.all(np.isfinite(value)):
        raise ValueError(f"{label or key} must be finite")
    return value


def _group_id_for_row(row: dict[str, Any]) -> str:
    segment_id = row.get("segment_id")
    if segment_id is not None:
        return f"segment:{segment_id}"
    return f"waypoint-block:{int(row['waypoint_id']) // 10}"


def _validate_rust_model(model: dict[str, Any]) -> None:
    if model.get("schema_version") != 1:
        raise ValueError(f"unsupported gravity model schema_version {model.get('schema_version')}")
    if model.get("model_kind") != MODEL_KIND:
        raise ValueError(f"unsupported gravity model kind {model.get('model_kind')}")
    if model.get("basis") != BASIS_TRIG_V1:
        raise ValueError(f"unsupported gravity basis {model.get('basis')}")
    if model.get("torque_convention") != TORQUE_CONVENTION:
        raise ValueError(f"unsupported torque convention {model.get('torque_convention')}")
    model_section = model.get("model", {})
    if model_section.get("feature_names") != trig_v1_feature_names():
        raise ValueError("gravity model feature_names do not match trig-v1 layout")
    coefficients = np.asarray(model_section.get("coefficients_nm"), dtype=np.float64)
    if coefficients.shape != (JOINT_COUNT, TRIG_V1_FEATURE_COUNT):
        raise ValueError(
            "expected coefficients_nm shape "
            f"({JOINT_COUNT}, {TRIG_V1_FEATURE_COUNT}), got {coefficients.shape}"
        )
    if not np.all(np.isfinite(coefficients)):
        raise ValueError("gravity model coefficients must be finite")


def _validate_fit_metadata(
    model: dict[str, Any],
) -> tuple[float, bool, float, list[str], list[str]]:
    fit = model.get("fit")
    if not isinstance(fit, dict):
        raise ValueError("fit must be an object")

    for key in (
        "ridge_lambda",
        "regularize_bias",
        "holdout_ratio",
        "train_group_ids",
        "holdout_group_ids",
    ):
        if key not in fit:
            raise ValueError(f"fit.{key} is required")

    ridge_lambda = _require_finite_number(fit["ridge_lambda"], "fit.ridge_lambda")
    if ridge_lambda <= 0.0:
        raise ValueError("fit.ridge_lambda must be > 0.0")
    regularize_bias = fit["regularize_bias"]
    if not isinstance(regularize_bias, bool):
        raise ValueError("fit.regularize_bias must be a boolean")
    holdout_ratio = _require_finite_number(fit["holdout_ratio"], "fit.holdout_ratio")
    if holdout_ratio < 0.0 or holdout_ratio >= 1.0:
        raise ValueError("fit.holdout_ratio must be in [0.0, 1.0)")

    train_group_ids = _require_string_list(
        fit["train_group_ids"], "fit.train_group_ids"
    )
    holdout_group_ids = _require_string_list(
        fit["holdout_group_ids"], "fit.holdout_group_ids"
    )
    return (
        ridge_lambda,
        regularize_bias,
        holdout_ratio,
        train_group_ids,
        holdout_group_ids,
    )


def _require_string_list(value: Any, label: str) -> list[str]:
    if not isinstance(value, list) or not all(isinstance(item, str) for item in value):
        raise ValueError(f"{label} must be a list of strings")
    return list(value)


def _residual_metrics(
    rows: list[dict[str, Any]], coefficients: np.ndarray
) -> dict[str, np.ndarray]:
    if not rows:
        return {
            "rms_residual_nm": np.zeros(JOINT_COUNT, dtype=np.float64),
            "p95_residual_nm": np.zeros(JOINT_COUNT, dtype=np.float64),
            "max_residual_nm": np.zeros(JOINT_COUNT, dtype=np.float64),
        }

    residuals = []
    for row in rows:
        features = trig_v1_features(np.array(row["q_rad"]))
        prediction = features @ coefficients
        residual = _joint_array(row, "tau_nm") - prediction
        if not np.all(np.isfinite(residual)):
            raise ValueError("fit produced non-finite residual")
        residuals.append(residual)

    residual_array = np.stack(residuals)
    abs_residual = np.abs(residual_array)
    return {
        "rms_residual_nm": np.sqrt(np.mean(residual_array * residual_array, axis=0)),
        "p95_residual_nm": np.array(
            [_percentile_from_sorted(np.sort(abs_residual[:, joint]), 0.95) for joint in range(6)]
        ),
        "max_residual_nm": np.max(abs_residual, axis=0),
    }


def _percentile_from_sorted(values: np.ndarray, quantile: float) -> float:
    if values.size == 0:
        return 0.0
    index = min(max(int(math.ceil(values.size * quantile)) - 1, 0), values.size - 1)
    return float(values[index])


def _residual_metric_max_abs_diff(
    train_metrics: dict[str, np.ndarray],
    holdout_metrics: dict[str, np.ndarray],
    fit_quality: dict[str, Any],
) -> float:
    comparisons = [
        ("rms_residual_nm", train_metrics["rms_residual_nm"]),
        ("p95_residual_nm", train_metrics["p95_residual_nm"]),
        ("max_residual_nm", train_metrics["max_residual_nm"]),
        ("holdout_rms_residual_nm", holdout_metrics["rms_residual_nm"]),
        ("holdout_p95_residual_nm", holdout_metrics["p95_residual_nm"]),
        ("holdout_max_residual_nm", holdout_metrics["max_residual_nm"]),
    ]
    max_diff = 0.0
    for key, python_values in comparisons:
        rust_values = _fit_quality_array(fit_quality, key)
        max_diff = max(max_diff, float(np.max(np.abs(python_values - rust_values))))
    return max_diff


def _fit_quality_array(fit_quality: dict[str, Any], key: str) -> np.ndarray:
    if not isinstance(fit_quality, dict):
        raise ValueError("fit_quality must be an object")
    if key not in fit_quality:
        raise ValueError(f"fit_quality.{key} is required")
    values = np.asarray(fit_quality[key], dtype=np.float64)
    if values.shape != (JOINT_COUNT,):
        raise ValueError(f"expected fit_quality.{key} to contain {JOINT_COUNT} values")
    if not np.all(np.isfinite(values)):
        raise ValueError(f"fit_quality.{key} must be finite")
    return values


def _synthetic_truth_coefficients() -> np.ndarray:
    coefficients = np.zeros((TRIG_V1_FEATURE_COUNT, JOINT_COUNT), dtype=np.float64)
    coefficients[0, :] = np.array([0.5, -0.25, 0.125, 0.0, 0.75, -0.5])
    coefficients[1, 0] = 2.0
    coefficients[3, 1] = -1.25
    coefficients[5, 2] = 0.9
    coefficients[14, 3] = -0.7
    coefficients[17, 4] = 1.1
    coefficients[22, 5] = 0.75
    return coefficients


def _synthetic_q(sample_index: int) -> np.ndarray:
    i = float(sample_index)
    return np.array(
        [
            math.sin((i * 0.017) + math.sin(i * 0.003)) * 1.2,
            math.cos((i * 0.023) + 0.4) * 1.1,
            math.sin((i * 0.031) + math.cos(i * 0.007)),
            math.cos((i * 0.037) + 0.8) * 0.9,
            math.sin((i * 0.041) + math.sin(i * 0.011)) * 1.3,
            math.cos((i * 0.047) + 1.2),
        ],
        dtype=np.float64,
    )


def _float_list(values: np.ndarray) -> list[float]:
    return [float(value) for value in values]


def _write_report(report: dict[str, Any], out: Path | None) -> None:
    text = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if out is None:
        sys.stdout.write(text)
        return
    if out.parent and str(out.parent) != ".":
        out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text, encoding="utf-8")


if __name__ == "__main__":
    raise SystemExit(main())
