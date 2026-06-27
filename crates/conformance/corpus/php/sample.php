<?php
function greet($name){return "hello $name";}

class Greeter {
public $name;
function __construct($name){$this->name=$name;}
}
