namespace Paper

/-- The sum `0 + 1 + ⋯ + n` of the first `n` positive integers. -/
def sumTo : Nat → Nat
  | 0 => 0
  | n + 1 => sumTo n + (n + 1)

/-- Gauss's formula, cited in `main.typ`: `2 · (1 + ⋯ + n) = n (n + 1)`. -/
theorem two_mul_sumTo (n : Nat) : 2 * sumTo n = n * (n + 1) := by
  induction n with
  | zero => rfl
  | succ k ih =>
    simp only [sumTo, Nat.mul_add, ih]
    grind

end Paper
