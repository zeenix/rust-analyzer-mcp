fn main() {
    let message = greet("World");
    println!("{}", message);

    let calc = Calculator::new();
    let result = calc.add(2, 3);
    println!("Result: {}", result);
}

fn greet(name: &str) -> String {
    format!("Hello, {}!", name)
}

struct Calculator {
    value: i32,
}

impl Calculator {
    fn new() -> Self {
        Self { value: 0 }
    }

    fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }

    fn multiply(&self, a: i32, b: i32) -> i32 {
        a * b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_greet() {
        assert_eq!(greet("Test"), "Hello, Test!");
    }

    #[test]
    fn test_calculator() {
        let calc = Calculator::new();
        assert_eq!(calc.add(2, 3), 5);
        assert_eq!(calc.multiply(3, 4), 12);
    }
}