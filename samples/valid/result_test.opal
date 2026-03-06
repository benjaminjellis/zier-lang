(extern let println ~ (String -> Unit) io/format)

(type ['e 'a] Result (
  (Ok ~ 'a)
  (Error ~ 'e)
))

(let safe_div {a b}
  (if (= b 0)
    (Error "division by zero")
    (Ok (/ a b))))

(let show_result {r}
  (match r
    (Ok _) ~> (println "ok~n")
    (Error _) ~> (println "error~n")))

(let main {}
  (show_result (safe_div 10 2))
  (show_result (safe_div 10 0)))
