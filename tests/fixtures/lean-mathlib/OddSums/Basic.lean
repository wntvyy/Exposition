import Mathlib.Algebra.BigOperators.Intervals
import Mathlib.Tactic.Ring

open Finset

/-- The sum of the first `n` odd numbers is `n ^ 2`. -/
theorem sum_odd (n : ℕ) : ∑ i ∈ range n, (2 * i + 1) = n ^ 2 := by
  induction n with
  | zero => simp
  | succ n ih =>
    rw [sum_range_succ, ih]
    ring
