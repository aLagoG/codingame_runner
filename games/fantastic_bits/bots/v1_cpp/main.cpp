// See games/tron/bots/baseline_cpp/main.cpp for the transport contract.

#include "strategy.h"

#include <iostream>

using namespace cgio;

#ifdef CGIO_RUST_SHIM
extern "C" int cgio_main()
#else
int main()
#endif
{
    std::ios_base::sync_with_stdio(false);

    InitialInput init;
    if (!(std::cin >> init)) return 0;
    fantastic_bits_v1_cpp::on_init(init);

    TurnInput input;
    while (std::cin >> input) {
        std::cout << fantastic_bits_v1_cpp::decide(input) << std::endl;
    }
    return 0;
}
