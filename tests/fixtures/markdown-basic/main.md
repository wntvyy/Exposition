---
title: A Markdown Print Profile
author:
  - Ada Lovelace
  - Alan Turing
date: November 11, 2026
papersize: a4
fontsize: 11pt
---

# Introduction

This *first* paragraph has **strong text**, a [link to Typst](https://typst.app),
inline `code`, and the inline equation $e^{i\pi} + 1 = 0$. Characters such as
\*stars\*, 50% & #hash, snake_case, 1 < 2 and a [bracket] print literally.

The second paragraph cites a footnote[^note] and uses the inline formula
$\alpha^2 + \beta_1 \le \sqrt{x}$ before it continues -- with dashes... and "quotes".

- First item with ~~struck~~ text
- Second item with $x_i$
  1. Nested ordered item
- [x] A finished task

| Symbol | Meaning         |   Value |
|:-------|:---------------:|--------:|
| $\pi$  | circle constant | 3.14159 |
| $e$    | Euler's number  | 2.71828 |

$$
\int_0^1 x^2 \, dx = \frac{1}{3}
$$

![A small test figure](figure.png)

```rust
fn main() {
    println!("hello");
}
```

> A block quote with *emphasis*.

---

Final paragraph with a hard  
line break.

[^note]: The footnote text with $n!$ inside.
