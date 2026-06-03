// See ../baseline_cpp/main.cpp for the transport contract.

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
    tron_v2_cpp::on_init(init);

    TurnInput input;
    while (std::cin >> input) {
        std::cout << tron_v2_cpp::decide(input) << std::endl;
    }
    return 0;
}
