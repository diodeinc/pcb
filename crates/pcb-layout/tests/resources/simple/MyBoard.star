load("stdlib.star", "I2C", "Power")

BMI270 = Module("BMI270.star")

p = Power("POWER")
i2c = I2C("I2C")

BMI270(
    name = "BMI270",
    power = p,
    i2c = i2c,
)

# HACK: Put the layout in build/ so that the snapshot ends up there, which
# is what the test expects.
add_property("layout_path", "build/")
