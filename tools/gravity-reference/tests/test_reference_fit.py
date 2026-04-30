import json

import numpy as np
import pytest

from gravity_fit_reference import (
    compare_model,
    load_samples,
    solve_ridge,
    trig_v1_feature_names,
    trig_v1_features,
)


def test_trig_v1_feature_order():
    q = np.array([0.0, np.pi / 2.0, 0.0, 0.0, 0.0, 0.0])
    phi = trig_v1_features(q)
    assert phi.shape == (23,)
    assert phi[0] == 1.0
    assert abs(phi[1] - 0.0) < 1e-12
    assert abs(phi[2] - 1.0) < 1e-12
    assert abs(phi[3] - 1.0) < 1e-12


def test_solve_ridge_recovers_known_coefficients():
    rng = np.random.default_rng(1)
    q = rng.uniform(low=-1.0, high=1.0, size=(300, 6))
    x = np.stack([trig_v1_features(row) for row in q])
    coeff = np.zeros((23, 6))
    coeff[1, 0] = 2.0
    coeff[3, 1] = -1.0
    y = x @ coeff
    solved = solve_ridge(x, y, ridge_lambda=1e-8, regularize_bias=False)
    assert np.max(np.abs(solved - coeff)) < 1e-6


def test_compare_rejects_missing_fit_quality_metrics():
    samples = _valid_samples()
    model = _valid_rust_model()
    del model["fit_quality"]["p95_residual_nm"]

    with pytest.raises(ValueError, match="fit_quality.p95_residual_nm"):
        compare_model(samples, model, _tolerances())


def test_compare_rejects_non_finite_fit_quality_metric_values():
    samples = _valid_samples()
    model = _valid_rust_model()
    model["fit_quality"]["rms_residual_nm"][2] = float("nan")

    with pytest.raises(ValueError, match="fit_quality.rms_residual_nm"):
        compare_model(samples, model, _tolerances())


def test_compare_rejects_missing_fit_metadata_instead_of_defaulting():
    samples = _valid_samples()
    model = _valid_rust_model()
    del model["fit"]["ridge_lambda"]

    with pytest.raises(ValueError, match="fit.ridge_lambda"):
        compare_model(samples, model, _tolerances())


def test_load_samples_rejects_invalid_schema_version(tmp_path):
    path = tmp_path / "samples.jsonl"
    header, rows = _valid_samples()
    header["schema_version"] = 2
    _write_jsonl(path, header, rows)

    with pytest.raises(ValueError, match="schema_version"):
        load_samples([path])


@pytest.mark.parametrize(
    ("key", "value"),
    [
        ("position_valid_mask", 256),
        ("raw_timestamp_us", "not-an-integer"),
    ],
)
def test_load_samples_rejects_invalid_mask_or_raw_timestamp_type(tmp_path, key, value):
    path = tmp_path / "samples.jsonl"
    header, rows = _valid_samples()
    rows[0][key] = value
    _write_jsonl(path, header, rows)

    with pytest.raises(ValueError, match=key):
        load_samples([path])


def _valid_samples():
    header = {
        "type": "header",
        "artifact_kind": "quasi-static-samples",
        "schema_version": 1,
        "source_path": "synthetic",
        "source_sha256": "synthetic",
        "role": "slave",
        "target": "synthetic",
        "joint_map": "piper_default",
        "load_profile": "unloaded",
        "torque_convention": "piper-sdk-normalized-nm-v1",
        "frequency_hz": 100.0,
        "max_velocity_rad_s": 0.08,
        "max_step_rad": 0.02,
        "settle_ms": 500,
        "sample_ms": 300,
        "stable_velocity_rad_s": 0.01,
        "stable_tracking_error_rad": 0.03,
        "stable_torque_std_nm": 0.08,
        "waypoint_count": 40,
        "accepted_waypoint_count": 40,
        "rejected_waypoint_count": 0,
    }
    rows = []
    for index in range(40):
        rows.append(
            {
                "type": "quasi-static-sample",
                "waypoint_id": index,
                "segment_id": f"segment-{index // 20}",
                "pass_direction": "forward",
                "host_mono_us": index * 10_000,
                "raw_timestamp_us": None,
                "q_rad": [index * 0.001, 0.0, 0.0, 0.0, 0.0, 0.0],
                "dq_rad_s": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                "tau_nm": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                "position_valid_mask": 63,
                "dynamic_valid_mask": 63,
                "stable_velocity_rad_s": 0.0,
                "stable_tracking_error_rad": 0.0,
                "stable_torque_std_nm": 0.0,
            }
        )
    return header, rows


def _valid_rust_model():
    return {
        "schema_version": 1,
        "model_kind": "joint-space-quasi-static-torque",
        "basis": "trig-v1",
        "role": "slave",
        "joint_map": "piper_default",
        "load_profile": "unloaded",
        "torque_convention": "piper-sdk-normalized-nm-v1",
        "created_at_unix_ms": 0,
        "sample_count": 40,
        "frequency_hz": 100.0,
        "fit": {
            "ridge_lambda": 1e-4,
            "regularize_bias": False,
            "solver": "cholesky",
            "fallback_solver": None,
            "holdout_strategy": "deterministic-group-stride-v1",
            "holdout_ratio": 0.0,
            "train_group_ids": ["segment:segment-0", "segment:segment-1"],
            "holdout_group_ids": [],
        },
        "training_range": {},
        "fit_quality": {
            "rms_residual_nm": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            "p95_residual_nm": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            "max_residual_nm": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            "holdout_rms_residual_nm": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            "holdout_p95_residual_nm": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            "holdout_max_residual_nm": [0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            "condition_number": 1.0,
        },
        "model": {
            "feature_names": trig_v1_feature_names(),
            "coefficients_nm": [[0.0] * 23 for _ in range(6)],
        },
    }


def _tolerances():
    return {"coefficient_atol": 1e-7, "residual_atol": 1e-7}


def _write_jsonl(path, header, rows):
    with path.open("w", encoding="utf-8") as f:
        f.write(json.dumps(header) + "\n")
        for row in rows:
            f.write(json.dumps(row) + "\n")
