# Plot n^2 for n in 0..9; run in the research session (Run File).
import matplotlib.pyplot as plt

xs = list(range(10))
plt.figure(figsize=(4, 3))
plt.plot(xs, [x * x for x in xs], marker="o")
plt.title("Squares")
plt.xlabel("n")
plt.ylabel("n^2")
plt.show()
