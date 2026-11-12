import LeanBasic.Defs

namespace LeanBasic

theorem double_add (a b : Nat) : double (a + b) = double a + double b := by
  unfold double
  omega

theorem add_zero' (n : Nat) : n + 0 = n := by
  sorry

theorem add_comm' (a b : Nat) : a + b = b + a := by
  skip

end LeanBasic
