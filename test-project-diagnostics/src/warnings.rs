// This file contains code that generates warnings but no errors.

#[allow(dead_code)]
fn used_function() {
    println!("This is used");
}

fn unused_function() {
    // Warning: function is never used
    println!("This is never called");
}

pub fn function_with_unused_variable() {
    let x = 5; // Warning: unused variable
    let y = 10;
    println!("y = {}", y);
}

pub fn function_with_unnecessary_mut() {
    let mut value = 42; // Warning: variable does not need to be mutable
    println!("Value: {}", value);
}

// Warning: struct is never constructed
struct UnusedStruct {
    field: i32,
}

pub fn unreachable_code_example() {
    return;
    println!("This is unreachable"); // Warning: unreachable code
}

pub fn main() {
    used_function();
    function_with_unused_variable();
    function_with_unnecessary_mut();
    unreachable_code_example();
}