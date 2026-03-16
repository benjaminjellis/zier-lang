# Testing
`test` is a language keyword. You do not import it.

Testing helpers are regular std functions and must be imported explicitly.

```mond
(use std/string)
(use std/result [Result])
(use std/testing [assert_eq])

(test "string/length"
  (let? [_ (assert_eq (string/length "hello") 5)]
    (assert_eq (string/length "") 0)))
```

Notes:

- `test` declarations are only allowed in files under `tests/`
- `let?` short-circuits on `Result`, no `bind` import required
- `(use std)` does not import `assert_eq` into unqualified scope
