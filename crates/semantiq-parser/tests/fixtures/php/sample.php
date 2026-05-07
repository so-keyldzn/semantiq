<?php
namespace App\Service;

use Foo\Bar;

class UserService {
    const VERSION = "1.0";

    public function greet(string $name): string {
        return "Hi $name";
    }
}

interface Greeter {
    public function greet(): string;
}

trait Loggable {
    public function log(string $msg): void {}
}

enum Status {
    case Active;
    case Inactive;
}

function helper(): int {
    return 1;
}
