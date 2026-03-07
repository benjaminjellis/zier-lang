-module(opal_testing_helpers).
-export([assert_eq/2, assert_ne/2]).

assert_eq(A, A) ->
    {ok, unit};
assert_eq(Expected, Got) ->
    Msg = lists:flatten(io_lib:format("expected ~p~n         got      ~p", [Expected, Got])),
    {error, Msg}.

assert_ne(A, B) when A =/= B ->
    {ok, unit};
assert_ne(A, _B) ->
    Msg = lists:flatten(io_lib:format("expected values to differ, but both were ~p", [A])),
    {error, Msg}.
