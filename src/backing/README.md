# skycurrent::backing

The `backing` module contains various APIs for supported backings.

Each backing typically has distinct characteristics which naturally point to working with them in a specific way. Some are naturally asynchronous and utilize green threads while others are naturally thread-bound. Some are capable of exposing extremely low-cost message stream filtering while others can only send and receive binary blobs.

The goal of these modules then is very important to note here. The goal is not to force these distinct backings to all behave in the exact same ways, whether that's by introducing unusual threading or a gazillion messaging channels or what else, and then getting a single consistent set of APIs across all backings.

Instead, each module here should simply provide the most natural and simple APIs to achieve the primary goals of ipc with SkyCurrent, which are:

1. To broadcast arbitrary-length binary blobs as singular units that can be sent in the lowest cost (time, CPU utilization) and fewest abstractions way possible.

2. To receive any such broadcasts of arbitrary-length binary blobs in a consistent (if a broadcast is sent, it should be received) and performant fashion (as much as possible).

