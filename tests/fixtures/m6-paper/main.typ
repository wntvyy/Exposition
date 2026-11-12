#set page(width: 15cm, height: 20cm, margin: 1.5cm)
#set par(justify: true)
#set heading(numbering: "1.")

= Triangular Numbers, Measured and Proved

== Introduction

The triangular numbers count the dots in a triangle with $n$ rows. This short paper measures their growth with a least-squares fit and proves the closed form in Lean.

== Main result

The sum of the first $n$ positive integers satisfies

$ sum_(k=1)^n k = (n (n + 1)) / 2 $

which is the lemma `Paper.two_mul_sumTo` in `Paper/Gauss.lean`.

== Numerical evidence

#figure(
  rect(width: 70%, height: 3cm, fill: luma(230)),
  caption: [Placeholder for the fitted curve.],
)

== Discussion

The quadratic term dominates for large $n$.
