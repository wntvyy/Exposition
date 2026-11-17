#set page(width: 14cm, height: 10cm, margin: 1.2cm)
#let theorem = (name, body) => block[*Theorem* (#name). #body]
#let lemma = (body) => block[*Lemma.* #body]
#let proof = (body) => block[_Proof._ #body]

= Introduction

The introduction states the setting. Keyword: alpha marker.

#theorem("Main result")[Every finite group is a group.] <thm:main>

#proof[Trivial, by alpha.]

== Background

Background text mentions alpha once more.

#lemma[An auxiliary statement about alpha.] <lem:aux>

#pagebreak()

= Results

The results section, also about alpha, ends here.
