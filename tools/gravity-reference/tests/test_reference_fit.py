import numpy as np

from gravity_fit_reference import trig_v1_features, solve_ridge


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
