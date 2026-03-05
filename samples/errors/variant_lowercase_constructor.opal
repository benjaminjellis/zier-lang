;; Error: constructor names must start with an uppercase letter
;; `none` and `some` are invalid — should be `None` and `Some`
(type ['a] Option (
  none
  (some ~ 'a)))
