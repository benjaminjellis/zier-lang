-module(zier_io_helpers).
-export([println/1]).

println(String) ->
  io:format("~ts~n", [String]).

