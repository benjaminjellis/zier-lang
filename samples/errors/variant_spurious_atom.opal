;; Error: `x` after `None` is not a valid constructor definition
;; Each entry in a variant body must be either a bare Name or (Name ~ Type)
(type ['a] Option ( None x (Some ~ 'a) ))
