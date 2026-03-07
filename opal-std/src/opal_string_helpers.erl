-module(opal_string_helpers).
-export([contains/2, concat/2, split/2]).

contains(Haystack, Needle) ->
    string:find(Haystack, Needle) =/= nomatch.

concat(A, B) -> <<A/binary, B/binary>>.

split(Str, Sep) -> string:split(Str, Sep, all).
