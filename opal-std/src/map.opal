;; map.opal — key/value maps backed by Erlang maps

(use option)

(pub extern type ['k 'v] Map maps/map)

;; Create an empty map.
(pub extern let new ~ (Map 'k 'v) maps/new)

;; Insert or update a key.
(pub extern let put ~ ('k -> 'v -> Map 'k 'v -> Map 'k 'v) maps/put)

;; Internal: only called after `has` confirms the key is present.
(extern let get_value ~ ('k -> Map 'k 'v -> 'v) maps/get)

;; Return true if the key is present.
(pub extern let has ~ ('k -> Map 'k 'v -> Bool) maps/is_key)

;; Lookup a key, returning None if absent.
(pub let get {k m}
  (if (has k m)
    (Some (get_value k m))
    None))

;; Remove a key (no-op if absent).
(pub extern let remove ~ ('k -> Map 'k 'v -> Map 'k 'v) maps/remove)

;; Number of entries.
(pub extern let size ~ (Map 'k 'v -> Int) maps/size)

;; Result of a take operation: the updated map and the removed value (if present).
(pub type ['k 'v] TakeResult (
  (:map   ~ Map 'k 'v)
  (:value ~ Option 'v)))

;; Remove a key and return both the updated map and the removed value.
(pub let take {k m}
  (if (has k m)
    (TakeResult :map (remove k m) :value (Some (get_value k m)))
    (TakeResult :map m            :value None)))
