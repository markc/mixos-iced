# Mix

Mix is the MixOS shell and scripting language. The interpreter lives in the
`mix` library; `cli/mix-shell` owns the binary, interactive shell and native ABP
citizen runtime. The current language version is 0.109.1 and the shell source
version is 0.109.4.

Use `mix builtins --names` to discover valid builtin names, and `mix builtins
<name>` for their contracts. `mix man overview` and `mix man syntax` explain the
language and command classifier. Discovery should inspect process status rather
than stop a sequence when a lookup is invalid.

Mix scripts use `run_argv` for local processes and native Bus verbs for application
control. `run_argv_must` is appropriate when a non-zero exit must stop the script.
The public language regression corpus accompanies the interpreter transplant.
