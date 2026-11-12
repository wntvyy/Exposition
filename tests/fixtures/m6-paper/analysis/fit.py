# Least-squares fit of the triangular numbers T(n) = 1 + 2 + ... + n.
# Run it in a research session (Run File): it shows one figure and one table.
import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

n = np.arange(1, 13)
t = n * (n + 1) // 2
coefficients = np.polyfit(n, t, deg=2)

plt.figure(figsize=(4, 3))
plt.plot(n, t, "o", label="T(n)")
plt.plot(n, np.polyval(coefficients, n), "-", label="quadratic fit")
plt.xlabel("n")
plt.ylabel("T(n)")
plt.title("Triangular numbers")
plt.legend()
plt.show()

pd.DataFrame({"term": ["n^2", "n", "1"], "coefficient": [round(float(c), 3) + 0.0 for c in coefficients]})
