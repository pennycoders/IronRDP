# IronRDP RDPSND

RDPSND static channel for audio output implemented as described in [MS-RDPEA].

This crate is part of the [IronRDP] project.

[IronRDP]: https://github.com/Devolutions/IronRDP
[MS-RDPEA]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpea/bea2d5cf-e3b9-4419-92e5-0e074ff9bc5b

Client and server support the pre-v8 WaveInfo / bare Wave two-message path in addition to Wave2.

The `pcm` module divides the sample rate of 16-bit PCM by 2 or 4, for servers whose clients do not accept their own rate.
