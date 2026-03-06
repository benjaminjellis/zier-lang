;; Declare an Erlang function with its Opal type.
;; Codegen will emit this as a direct call to the Erlang target on the BEAM.
(extern let println ~ (String -> Unit) io/format)

(let greet {} (println "Hello, world!~n"))

(let main {} (greet))
