(use std)
(use std/result)

(test "string/length"
  (let? [_ (testing/assert_eq (string/length "hello") 5)]
    (testing/assert_eq (string/length "") 0)))

(test "string/is_empty"
  (let? [_ (testing/assert (string/is_empty ""))]
    (testing/assert_ne (string/is_empty "hi") True)))

(test "string/trim"
  (testing/assert_eq (string/trim "  hello  ") "hello"))

(test "string/uppercase"
  (testing/assert_eq (string/uppercase "hello") "HELLO"))

(test "string/lowercase"
  (testing/assert_eq (string/lowercase "HELLO") "hello"))

(test "string/casefold"
  (testing/assert_eq (string/casefold "HELLO") "hello"))

(test "string/concat"
  (testing/assert_eq (string/concat "hello" " world") "hello world"))

(test "string/contains"
  (let? [_ (testing/assert (string/contains "hello world" "world"))]
    (testing/assert_ne (string/contains "hello world" "xyz") True)))

(test "string/split"
  (testing/assert_eq (string/split "a,b,c" ",") ["a" "b" "c"]))
