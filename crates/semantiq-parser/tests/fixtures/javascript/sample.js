import { foo } from "./bar";

class Calculator {
    constructor(initial) {
        this.value = initial;
    }

    add(n) {
        return this.value + n;
    }
}

const multiply = (a, b) => a * b;
const settings = { debug: true };

function processData(items) {
    return items.map((x) => x * 2);
}
