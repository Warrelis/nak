# Nak

Ever been frustrated that your aliases aren’t available on every server you SSH to? Don’t like the default bash command keybindings? Trying to `<option-left>` to skip-by-word, only to find that you end up with `;9D` junk in your terminal instead? Worry no more!

TL;DR: Nak is a new unix shell - but it’s also a whole new way to build shells.  Nak focuses on centralizing all user customization options on the original host, and respects those customizations even across ssh connections.

To accomplish this, Nak separates the shell into two processes, in two separate binaries:

- the frontend, which is responsible for all user customization (parsing, keyboard shortcuts, aliases, editor preferences, etc); and…
- the backend, which is exceedingly simple, and responsible only for responding to json-rpc messages.

These processes communicate via json-rpc over `stdin` and `stdout`, which makes it just as easy to have the backend running on the other end of an ssh connection, as it is to have it running locally.

One of the central goals of Nak is to make it easy to implement new frontends and backends. Implementing a new frontend allows developers to further customize the experience, and adding new backends allows Nak’s customization to follow any user into new environments.

Built by joshuawarner32@gmail.com in San Francisco, with 💜 and :rust:.

## License
Unless otherwise noted:

```
Copyright 2018 Dropbox, Inc.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
```
