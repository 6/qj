module {name: "loc", deps: "overridden by modulemeta"};
import "inner" as inner {search: "./nested"};
include "inner" {search: "./nested"};

# $__loc__ in a module names the module's file.
def loc: $__loc__;
def both: [loc, inner::here, here];
def _private: 1;
