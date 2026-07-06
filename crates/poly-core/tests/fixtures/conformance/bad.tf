resource "aws_instance" "bad" {
  ami           = "ami-12345678"
  instance_type = "t2.micro"
