#include <stdio.h>

struct Point {
    int x;
    int y;
};

enum Color {
    RED,
    GREEN
};

typedef struct Point Point;

int add(int a, int b) {
    return a + b;
}

int* make_buf(size_t n) {
    return 0;
}

int main(void) {
    return 0;
}
