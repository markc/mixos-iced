# Handler closure capture

Lambdas defined inside an `on` handler capture its local variables by value,
including callbacks passed to `map` and `all` and nested callbacks. Those
locals remain available if the closure escapes the handler. Global variables
continue to be read live; capturing handler locals does not freeze globals.

Each handler invocation has its own local frame. A closure retains the locals
of its defining invocation, rather than reading another invocation's locals.
Assignment inside a closure continues to bind a function-local variable.
