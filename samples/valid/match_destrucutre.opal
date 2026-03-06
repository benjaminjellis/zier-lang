(let check {x y}
  (match x
    y 10 10 ~> (println "both 10")
    _ _ ~> (println "not the same")))

(let check_two {x}
  (match x
    10 or 11 or 12 ~> (println "expected")
    _ ~> (println "not expected")))

(let main {} (let [x 10 y 10] (check x y)))
