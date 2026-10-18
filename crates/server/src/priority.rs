//! Which job may overwrite what the others wrote, so that they do not undo one another.
//!
//! The author of a row is the last one who wrote it. The jobs that change what a text says have an
//! order, from the highest:
//! 1. `names`: which names stay in English (a decision of the project);
//! 2. `byhand`: the review and the plurals answered by hand (byhand.rs), a person's judgment;
//! 3. `consistency`: one form for a name across the texts;
//! 4. `review`: the meaning (a language model's opinion, the weakest of them);
//! then the machine translation of an addon or the server.
//! A job leaves alone the rows of the ones above it. Hand corrections (trust 2 and 3) stay out of
//! reach of all of them. `repair` and `plurals` are outside the order: they change the form (tags,
//! quotes, plural marks) and not what the text says, and their checks see to it, so they go on top
//! of any row, and a row of the order keeps its author.

/// The author of the rows answered by hand.
pub const BY_HAND: &str = "byhand";

/// The authors whose rows the review does not rewrite (nor asks the model about).
pub const ABOVE_REVIEW: &str = "'names', 'byhand', 'consistency'";

/// The authors whose rows the consistency job does not rewrite.
pub const ABOVE_CONSISTENCY: &str = "'names', 'byhand'";
