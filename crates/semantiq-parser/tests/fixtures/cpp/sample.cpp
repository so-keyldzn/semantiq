namespace ns {

class Calculator {
public:
    int add(int n) { return n + 1; }
    int subtract(int n) { return n - 1; }
    ~Calculator() {}
};

struct Point {
    int x;
    int y;
};

}

int ns::Calculator_external() { return 0; }

int main() {
    return 0;
}
