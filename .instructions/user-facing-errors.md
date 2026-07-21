# User-facing error messages

- Error messages for **user-facing** surfaces must be clear and easy to understand.
- Always offer **possible next steps** the user can take.
- When useful, mention **common misconfigurations or mistakes** that often cause this error.
- Include **relevant context** that helps the user fix the problem (paths, IDs, which check failed, which value was rejected, which dependency is missing).
- Surfaces that count as user-facing: command-line programs, graphical UIs, installers, and HTTP APIs or status pages meant for human operators.
- The only reason to omit extra diagnostic context is when gathering it is **performance-expensive** and the code is a **library** used in hot paths — not a user-facing tool. In that case prefer a clear message plus a documented way to get more detail (flag, log level, or error code).
