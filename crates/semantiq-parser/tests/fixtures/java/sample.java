import java.util.List;

public class Calculator {
    private int value;

    public Calculator(int v) {
        this.value = v;
    }

    public int add(int n) {
        return value + n;
    }
}

interface Computable {
    int compute();
}

enum Status {
    ACTIVE,
    INACTIVE
}
