// SPDX-License-Identifier: GPL-2.0-only

#pragma once

#include <iostream>

inline int failures = 0;

#define CHECK(condition)                                                                   \
    do                                                                                     \
    {                                                                                      \
        if(!(condition))                                                                   \
        {                                                                                  \
            std::cerr << __FILE__ << ":" << __LINE__ << ": CHECK(" #condition ") failed\n"; \
            ++failures;                                                                    \
        }                                                                                  \
    } while(false)

inline int finish(const char* what)
{
    if(failures != 0)
    {
        std::cerr << failures << " check(s) failed\n";
        return 1;
    }
    std::cout << what << ": all checks passed\n";
    return 0;
}
