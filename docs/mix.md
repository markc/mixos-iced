# Mix

Mix is the MixOS shell and scripting language. The interpreter lives in the
`mix` library; `cli/mix-shell` owns the binary, interactive shell and native ABP
citizen runtime. The current language version is 0.109.1 and the shell source
version is 0.109.5.

Use `mix builtins --names` to discover valid builtin names, and `mix builtins
<name>` for their contracts. `mix man overview` and `mix man syntax` explain the
language and command classifier. Discovery should inspect process status rather
than stop a sequence when a lookup is invalid.

Mix scripts use `run_argv` for local processes and native Bus verbs for application
control. `run_argv_must` is appropriate when a non-zero exit must stop the script.
The public language regression corpus accompanies the interpreter transplant.

Uncaught script errors print a diagnostic and exit 1, including messages with
`interrupted:false` or the word `interrupted`. Signal outcomes come from actual
SIGINT/SIGTERM delivery: non-interactive scripts and `-c` exit 130/143. SIGINT
on an interactive terminal retains exit 0. Script-defined error codes do not
impersonate OS signals; `--serve` retains its graceful shutdown behaviour.
